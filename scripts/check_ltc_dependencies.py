#!/usr/bin/env python3
"""Check each SDK's active runtime graph: LTC must not introduce devices or GPL/LGPL."""
import argparse
import json
import re
import subprocess


def check(target, native):
    features = 'tidkod-bindings/c' + (',tidkod-bindings/native' if native else '')
    metadata = json.loads(subprocess.check_output([
        'cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', target,
        '--no-default-features', '--features', features,
    ], encoding='utf-8'))
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    root = next(p['id'] for p in packages.values() if p['name'] == 'tidkod-bindings')
    pending, seen = [root], set()
    while pending:
        key = pending.pop()
        if key in seen:
            continue
        seen.add(key)
        p = packages[key]
        if p['name'] in {'cpal', 'alsa', 'alsa-sys', 'coreaudio-rs', 'libltc', 'ltc', 'x42ltc-sys'}:
            raise RuntimeError(f"Device/reference dependency in shipped graph: {p['name']}")
        if re.search(r'\b(?:A?GPL|LGPL)-', p.get('license') or ''):
            raise RuntimeError(f"GPL/LGPL license in shipped graph: {p['name']}: {p['license']}")
        pending.extend(d['pkg'] for d in nodes[key]['deps'] if any(k['kind'] is None for k in d['dep_kinds']))
    names = {packages[p]['name'] for p in seen}
    if not {'tidkod-protocol', 'st12-1', 'broadcast-common'} <= names:
        raise RuntimeError('Missing portable LTC dependencies')
    print(f"{'native' if native else 'core'} / {target}: {len(seen)} runtime packages, no devices or GPL/LGPL")


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target')
    args = parser.parse_args()
    target = args.target or next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], encoding='utf-8').splitlines() if line.startswith('host: '))
    for native in [False, True]:
        check(target, native)
