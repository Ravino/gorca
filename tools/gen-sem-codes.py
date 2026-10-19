# Regenerates crates/gorca-ext-api/src/codes.rs and codes.snapshot from the oracle tables in
# tests/fixtures/fuse/fuse-x32.json and the role table in gorca-model. Usage: gen-sem-codes.py <repo root>.
# The snapshot is append-only: after running, review the diff; changed or removed lines are a bug.
import re, json, sys
root = sys.argv[1]
d = json.load(open(root + '/tests/fixtures/fuse/fuse-x32.json'))['tables']
roles = re.findall(r'(\w+) = (\d+) => "([^"]+)"', open(root + '/crates/gorca-model/src/sem/role.rs').read())
reg = {}
reg['groups'] = sorted(((k, v) for k, v in d['group_codes'].items() if v < 0x8000), key=lambda x: x[1])
reg['evidence'] = sorted(((k, v) for k, v in d['group_codes'].items() if v >= 0x8000), key=lambda x: x[1])
reg['prov'] = sorted(d['prov_codes'].items(), key=lambda x: x[1])
reg['conf'] = sorted(d['conf_codes'].items(), key=lambda x: x[1])
reg['bind'] = sorted(d['bind_quality'].items(), key=lambda x: x[1])
reg['source_class'] = sorted(d['class_codes'].items(), key=lambda x: x[1])
reg['tier'] = [('T0', 0), ('T1', 1), ('T2', 2), ('T3', 3)]
reg['role'] = [(n, int(c)) for _, c, n in roles]
out = ['// SPDX-License-Identifier: MIT OR Apache-2.0',
       '//! Name <-> code registries of the semantic model: a **generated copy** of the tables in',
       '//! `gorca-model::sem::registry` (the source of truth). This crate cannot depend on `gorca-model`;',
       '//! the equality test below (dev-dependency) and the checked-in append-only snapshot',
       '//! `codes.snapshot` keep the two in step. Codes are never reused or renumbered; a new entry is an',
       '//! append to the model, to these tables and to the snapshot.', '',
       '/// One registry row: registry name and wire code.', "pub type Row = (&'static str, u32);", '']
names = {'groups': 'GROUPS', 'evidence': 'EVIDENCE', 'prov': 'PROV_CODES', 'conf': 'CONF_CLASSES',
         'bind': 'BIND_QUALITIES', 'source_class': 'SOURCE_CLASSES', 'tier': 'TRUST_TIERS', 'role': 'ROLES'}
docs = {'groups': 'Fused semantic groups (code = slot).', 'evidence': 'Evidence kinds (bit 15 set).',
        'prov': 'Provenance codes (4 bits in a provenance tag; no `Fallback`).',
        'conf': 'Confidence classes.', 'bind': 'Binding qualities.', 'source_class': 'Source classes.',
        'tier': 'Trust tiers.', 'role': 'Semantic roles.'}
for k, n in names.items():
    out.append('/// ' + docs[k])
    out.append(f'pub const {n}: &[Row] = &[')
    for a, b in reg[k]:
        out.append(f'    ("{a}", {b}),')
    out.append('];')
    out.append('')
out.append('/// Every registry as `(registry, rows)`, in snapshot order.')
out.append('pub const REGISTRIES: &[(&str, &[Row])] = &[')
for k, n in names.items():
    out.append(f'    ("{k}", {n}),')
out.append('];')
open(root + '/crates/gorca-ext-api/src/codes.rs', 'w').write('\n'.join(out) + '\n')
snap = ['# Append-only snapshot of the semantic registries (see src/codes.rs).',
        '# Format: <registry> <name> <code>. Lines may be added, never changed or removed.']
for k in names:
    for a, b in reg[k]:
        snap.append(f'{k} {a} {b}')
open(root + '/crates/gorca-ext-api/codes.snapshot', 'w').write('\n'.join(snap) + '\n')
