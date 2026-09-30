#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Isolated daemon IPC acceptance test; uses dummy input, no OS clipboard."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/lan-mouse').resolve()
assert sys.platform != 'win32', 'This isolation test requires Unix IPC sockets.'

def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='lan-bridge-ui-') as directory:
    root = Path(directory)
    config = root / 'config.toml'
    port = free_port()
    config.write_text(f'port = {port}\nclipboard = false\ndiscovery = false\ncapture_backend = "dummy"\nemulation_backend = "dummy"\n')
    env = {**os.environ, 'LAN_MOUSE_IPC_SOCKET': str(root / 'ipc.sock')}
    with (root / 'daemon.log').open('w+') as logfile:
        daemon = subprocess.Popen([str(binary), '--config', str(config), '--cert-path', str(root/'cert.pem'), 'daemon'], env=env, stdout=logfile, stderr=logfile)
        try:
            for _ in range(100):
                if (root/'ipc.sock').exists():
                    break
                if daemon.poll() is not None:
                    raise RuntimeError('daemon failed to start')
                time.sleep(.05)
            sequence = 0
            def request(action, fails=False):
                global sequence
                sequence += 1
                correlation = str(sequence)
                with socket.socket(socket.AF_UNIX) as sock:
                    sock.settimeout(12)
                    sock.connect(str(root/'ipc.sock'))
                    sock.sendall((json.dumps({'Ui': {'id': correlation, 'action': action}})+'\n').encode())
                    with sock.makefile('r') as stream:
                        for line in stream:
                            event=json.loads(line).get('UiResult')
                            if event and event['id']==correlation:
                                result=event['result']
                                if fails:
                                    assert 'Err' in result, result
                                    return result['Err']
                                assert 'Ok' in result, result
                                return result['Ok']
                    raise RuntimeError('no operation acknowledgement')
            initial=request({'type':'snapshot'})
            assert initial['protocol_version']==6 and not initial['clients']
            fp=':'.join(['ab']*32)
            request({'type':'add_client','hostname':None,'ips':['127.0.0.1'],'port':port,'position':'right','fingerprint':'123456'}, fails=True)
            assert not request({'type':'snapshot'})['clients']
            added=request({'type':'add_client','hostname':None,'ips':['127.0.0.1'],'port':port,'position':'right','fingerprint':fp})
            client=added['clients'][0][0]
            assert added['authorized'][fp]=='127.0.0.1'
            hooked=request({'type':'set_hooks','id':client,'enter_hook':'echo enter','leave_hook':'echo leave'})
            assert hooked['clients'][0][1]['cmd']=='echo enter' and hooked['clients'][0][1]['leave_cmd']=='echo leave'
            edited=request({'type':'update_client','id':client,'hostname':None,'ips':['127.0.0.1'],'port':port,'position':'right','active':True})
            assert edited['clients'][0][1]['cmd']=='echo enter' and edited['clients'][0][1]['leave_cmd']=='echo leave', 'editing network configuration lost hooks'
            cleared=request({'type':'set_hooks','id':client,'enter_hook':None,'leave_hook':None})
            assert cleared['clients'][0][1]['cmd'] is None and cleared['clients'][0][1]['leave_cmd'] is None
            request({'type':'save_config'})
            request({'type':'set_active','id':client,'active':False})
            moved=request({'type':'set_position','id':client,'position':'left'})
            assert moved['clients'][0][1]['pos']=='left' and not moved['clients'][0][2]['active']
            assert request({'type':'set_paused','paused':True})['paused']
            assert not request({'type':'set_paused','paused':False})['paused']
            assert request({'type':'release'})['active_client'] is None
            second=request({'type':'add_client','hostname':'localhost','ips':['127.0.0.1'],'port':port,'position':'right','fingerprint':None})['clients'][-1][0]
            request({'type':'set_active','id':client,'active':True})
            swapped=request({'type':'set_position','id':client,'position':'right'})
            states={i:(c['pos'],s['active']) for i,c,s in swapped['clients']}
            assert states[client]==('right',True) and states[second]==('left',True), states
            with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as occupied:
                occupied.bind(('0.0.0.0',0))
                conflict=occupied.getsockname()[1]
                request({'type':'set_settings','port':conflict,'clipboard':False},fails=True)
                assert request({'type':'snapshot'})['port']==port
            replacement=free_port()
            saved=request({'type':'set_settings','port':replacement,'clipboard':False})
            assert saved['port']==replacement and f'port = {replacement}' in config.read_text()
            request({'type':'remove_client','id':second})
            assert not request({'type':'remove_client','id':client})['clients']
            assert '[[clients]]' not in config.read_text(), 'last client was not removed from config'
            assert fp in request({'type':'snapshot'})['authorized'], 'removing config silently revoked identity'
            assert not request({'type':'revoke','fingerprint':fp})['authorized']
            request({'type':'shutdown'})
            assert daemon.wait(timeout=5)==0
            # Restart proves persisted configuration, including last-device deletion.
            daemon=subprocess.Popen([str(binary),'--config',str(config),'--cert-path',str(root/'cert.pem'),'daemon'],env=env,stdout=logfile,stderr=logfile)
            for _ in range(100):
                try:
                    restored=request({'type':'snapshot'})
                    break
                except (ConnectionRefusedError,FileNotFoundError):
                    time.sleep(.05)
            assert not restored['clients'] and not restored['authorized'] and restored['port']==replacement
            request({'type':'shutdown'})
            assert daemon.wait(timeout=5)==0
            print('PASS: validation, authorization, hooks/update/clear, inactive drag, edge swap, pause/resume, release, port conflict, save/restart, last-device deletion, shutdown')
        except Exception:
            logfile.flush()
            logfile.seek(0)
            print(logfile.read(),file=sys.stderr)
            raise
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                daemon.wait(timeout=5)
