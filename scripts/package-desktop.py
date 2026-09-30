#!/usr/bin/env python3
# DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
"""Package an already built macOS binary into a local, ad-hoc-signed .app."""
from pathlib import Path
import argparse
import plistlib
import shutil
import subprocess
import sys

if sys.platform!='darwin':
    raise SystemExit('macOS packaging only; Windows runs target/release/lan-mouse.exe')
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',nargs='?',default='target/release/lan-mouse')
parser.add_argument('--output',default='target/DeskUnify.app',help='application bundle destination')
parser.add_argument('--zip',dest='archive',help='optional ZIP containing the application bundle')
args=parser.parse_args()
binary=Path(args.binary).resolve()
if not binary.is_file():
    raise SystemExit(f'Build the egui binary first: {binary}')
app=Path(args.output).resolve()
if app.suffix != '.app':
    raise SystemExit('Output must be a .app bundle')
contents=app/'Contents'
(contents/'MacOS').mkdir(parents=True,exist_ok=True)
(contents/'Resources').mkdir(parents=True,exist_ok=True)
shutil.copy2(binary,contents/'MacOS'/'lan-mouse')
shutil.copy2('lan-mouse-egui/icons/icon.icns',contents/'Resources'/'icon.icns')
shutil.copy2('LICENSE',contents/'Resources'/'LICENSE')
with (contents/'Info.plist').open('wb') as output:
    plistlib.dump({'CFBundleIconFile':'icon.icns','CFBundleExecutable':'lan-mouse','CFBundleIdentifier':'dev.lanbridge.desktop','CFBundleName':'DeskUnify','CFBundleDisplayName':'DeskUnify','CFBundlePackageType':'APPL','CFBundleShortVersionString':'0.1.0','CFBundleVersion':'1','NSHighResolutionCapable':True,'NSLocalNetworkUsageDescription':'Discover and connect to DeskUnify computers on your local network.','NSBonjourServices':['_lanbridge._udp'],'NSInputMonitoringUsageDescription':'Share keyboard and mouse input with authorized devices on your local network.','NSAccessibilityUsageDescription':'Capture and replay keyboard and mouse input for local network sharing.'},output)
subprocess.run(['codesign','--force','--deep','--sign','-',str(app)],check=True)
print(app)
if args.archive:
    archive=Path(args.archive).resolve()
    archive.parent.mkdir(parents=True,exist_ok=True)
    archive_base=archive.with_suffix('') if archive.suffix=='.zip' else archive
    print(shutil.make_archive(str(archive_base),'zip',root_dir=app.parent,base_dir=app.name))
