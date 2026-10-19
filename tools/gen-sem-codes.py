# Regenerates crates/gorca-ext-api/src/codes.rs and extends codes.snapshot from the oracle tables in
# tests/fixtures/fuse/fuse-x32.json and the enum tables in gorca-model (sem/role.rs, sem/states.rs).
# Usage: gen-sem-codes.py <repo root>
#
# The snapshot is APPEND-ONLY. The generator refuses (exit 1) when any row already in the snapshot is
# changed or missing from the current tables, and only ever appends new rows at the end of the file.
import json
import re
import sys

root = sys.argv[1]
fx = json.load(open(root + '/tests/fixtures/fuse/fuse-x32.json'))['tables']
model = root + '/crates/gorca-model/src/sem/'


def enum_rows(path, name):
    src = open(path).read()
    m = re.search(r'pub enum ' + name + r': \w+ \{(.*?)\n    \}', src, re.S)
    if not m:
        sys.exit(f'enum {name} not found in {path}')
    return [(t, int(c)) for _, c, t in re.findall(r'(\w+) = (\d+) => "([^"]+)"', m.group(1))]


def flag_rows(path, name):
    src = open(path).read()
    m = re.search(r'pub struct ' + name + r': \w+ \{(.*?)\n    \}', src, re.S)
    if not m:
        sys.exit(f'flags {name} not found in {path}')
    return [(n, 1 << int(sh)) for n, sh in re.findall(r'const (\w+) = 1 << (\d+);', m.group(1))]


reg = {}
# Groups: the oracle's, then the ones appended to the model after the corpus was generated (M22a-C3:
# roledescription .. namefrom). Appended groups have no oracle rows; the model is their source.
_oracle_groups = {k: v for k, v in fx['group_codes'].items() if v < 0x8000}
_appended = [(k, v) for k, v in enum_rows(model + 'codes.rs', 'Group') if k not in _oracle_groups]
for _k, _v in _appended:
    if _v in _oracle_groups.values():
        sys.exit(f'appended group {_k} reuses code {_v}')
reg['groups'] = sorted(list(_oracle_groups.items()) + _appended, key=lambda x: x[1])
reg['evidence'] = sorted(((k, v) for k, v in fx['group_codes'].items() if v >= 0x8000), key=lambda x: x[1])
reg['prov'] = sorted(fx['prov_codes'].items(), key=lambda x: x[1])
reg['conf'] = sorted(fx['conf_codes'].items(), key=lambda x: x[1])
reg['bind'] = sorted(fx['bind_quality'].items(), key=lambda x: x[1])
reg['source_class'] = sorted(fx['class_codes'].items(), key=lambda x: x[1])
reg['tier'] = [('T0', 0), ('T1', 1), ('T2', 2), ('T3', 3)]
reg['role'] = enum_rows(model + 'role.rs', 'SemRole')
for key, ty in [('check', 'Check'), ('press', 'Press'), ('expand', 'Expand'), ('select', 'Select'),
                ('current', 'Current'), ('invalid', 'Invalid'), ('popup', 'Popup'), ('avail', 'Avail'),
                ('name_from', 'NameFrom'), ('orientation', 'Orientation'), ('sort', 'Sort'),
                ('pos_src', 'PosSrc')]:
    reg[key] = enum_rows(model + 'states.rs', ty)
reg['sem_flags'] = flag_rows(model + 'states.rs', 'SemFlags')
reg['relevant'] = flag_rows(model + 'states.rs', 'Relevant')

names = {'groups': 'GROUPS', 'evidence': 'EVIDENCE', 'prov': 'PROV_CODES', 'conf': 'CONF_CLASSES',
         'bind': 'BIND_QUALITIES', 'source_class': 'SOURCE_CLASSES', 'tier': 'TRUST_TIERS',
         'role': 'ROLES', 'check': 'CHECK', 'press': 'PRESS', 'expand': 'EXPAND', 'select': 'SELECT',
         'current': 'CURRENT', 'invalid': 'INVALID', 'popup': 'POPUP', 'avail': 'AVAIL',
         'name_from': 'NAME_FROM', 'orientation': 'ORIENTATION', 'sort': 'SORT', 'pos_src': 'POS_SRC',
         'sem_flags': 'SEM_FLAGS', 'relevant': 'RELEVANT'}
docs = {'groups': 'Fused semantic groups (code = slot).', 'evidence': 'Evidence kinds (bit 15 set).',
        'prov': 'Provenance codes (4 bits in a provenance tag; no `Fallback`).',
        'conf': 'Confidence classes.', 'bind': 'Binding qualities.', 'source_class': 'Source classes.',
        'tier': 'Trust tiers.', 'role': 'Semantic roles.', 'check': '`aria-checked` states.',
        'press': '`aria-pressed` states.', 'expand': '`aria-expanded` states.',
        'select': '`aria-selected` states.', 'current': '`aria-current` values.',
        'invalid': '`aria-invalid` values.', 'popup': '`aria-haspopup` values.',
        'avail': 'Availability values.', 'name_from': 'Name sources.',
        'orientation': 'Orientation values.', 'sort': 'Sort values.',
        'pos_src': 'Set-position sources.',
        'sem_flags': '`SemFlags` bit values (code = bit mask).', 'relevant': '`Relevant` bit values.'}

# Append-only guard.
snap_path = root + '/crates/gorca-ext-api/codes.snapshot'
try:
    old_lines = open(snap_path).read().splitlines()
except FileNotFoundError:
    old_lines = ['# Append-only snapshot of the semantic registries (see src/codes.rs).',
                 '# Format: <registry> <name> <code>. Lines may be added, never changed or removed.']
current = {f'{k} {a} {b}' for k in names for a, b in reg[k]}
old_rows = [l for l in old_lines if l.strip() and not l.startswith('#')]
broken = [l for l in old_rows if l not in current]
if broken:
    sys.exit('REFUSED: snapshot rows changed or removed (codes are append-only):\n  ' + '\n  '.join(broken))

out = ['// SPDX-License-Identifier: MIT OR Apache-2.0',
       '//! Name <-> code registries of the semantic model: a **generated copy** of the tables in',
       '//! `gorca-model::sem::registry` (the source of truth). This crate cannot depend on `gorca-model`;',
       '//! the equality test (dev-dependency) and the checked-in append-only snapshot',
       '//! `codes.snapshot` keep the two in step. Codes are never reused or renumbered; a new entry is an',
       '//! append to the model, to these tables and to the snapshot (`tools/gen-sem-codes.py`).', '',
       '/// One registry row: registry name and wire code.', "pub type Row = (&'static str, u32);", '']
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

seen = set(old_rows)
new = []
for k in names:
    for a, b in reg[k]:
        line = f'{k} {a} {b}'
        if line not in seen:
            new.append(line)
open(snap_path, 'w').write('\n'.join(old_lines + new) + '\n')
print(f'snapshot: {len(old_rows)} kept, {len(new)} appended')
