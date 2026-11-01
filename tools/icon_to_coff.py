"""Собирает assets/icon.o (COFF-объект с секцией .rsrc): иконка из assets/icon.ico и версия из Cargo.toml
(«Свойства → Подробно» у exe).

Так ресурсы попадают в .exe без windres/rc: build.rs передаёт icon.o линковщику.
Запуск после замены иконки или смены версии (tools/release.ps1 запускает сам):  python tools/icon_to_coff.py
"""
import re
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RT_ICON, RT_GROUP_ICON, RT_VERSION, LANG = 3, 14, 16, 0x0409


def cargo_version():
    toml = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    return re.search(r'^version\s*=\s*"([^"]+)"', toml, re.M).group(1)


def ver_node(key, value=b'', wtype=1, children=(), value_len=None):
    """Узел VS_VERSIONINFO: длина, длина значения, тип, ключ UTF-16, выравнивание по 4, значение, дети."""
    def pad(b):
        return b + b'\0' * (-len(b) % 4)
    body = pad(struct.pack('<HHH', 0, 0, wtype) + (key + '\0').encode('utf-16-le'))
    if value:
        body = pad(body + value)
    for c in children:
        body = pad(body) + c
    vlen = value_len if value_len is not None else len(value)
    return struct.pack('<HH', len(body), vlen) + body[4:]


def version_info(version):
    nums = [int(x) for x in re.findall(r'\d+', version)[:4]] + [0] * 4
    ms, ls = nums[0] << 16 | nums[1], nums[2] << 16 | nums[3]
    fixed = struct.pack('<13I', 0xFEEF04BD, 0x00010000, ms, ls, ms, ls, 0x3F, 0, 0x40004, 1, 0, 0, 0)
    strings = [('CompanyName', 'finboard-anon'), ('FileDescription', 'Обезличивание Excel'), ('FileVersion', version),
               ('InternalName', 'finboard-anon'), ('LegalCopyright', 'GPL-3.0-only'), ('ProductName', 'finboard-anon'),
               ('ProductVersion', version)]
    table = ver_node('041904B0', children=[ver_node(k, (v + '\0').encode('utf-16-le'), 1, value_len=len(v) + 1) for k, v in strings])
    sfi = ver_node('StringFileInfo', children=[table])
    vfi = ver_node('VarFileInfo', children=[ver_node('Translation', struct.pack('<HH', 0x0419, 1200), 0)])
    return ver_node('VS_VERSION_INFO', fixed, 0, [sfi, vfi])


def read_ico(path):
    ico = path.read_bytes()
    _, _, count = struct.unpack_from('<HHH', ico, 0)
    out = []
    for i in range(count):
        w, h, cc, _, planes, bits, size, off = struct.unpack_from('<BBBBHHII', ico, 6 + 16 * i)
        out.append((w, h, cc, planes, bits, ico[off:off + size]))
    return out


def build(images, version):
    group = struct.pack('<HHH', 0, 1, len(images))
    for i, (w, h, cc, planes, bits, data) in enumerate(images, start=1):
        group += struct.pack('<BBBBHHIH', w, h, cc, 0, planes, bits, len(data), i)
    # тип -> [(id, данные)], типы и id по возрастанию
    tree = [(RT_ICON, [(i, img[5]) for i, img in enumerate(images, start=1)]),
            (RT_GROUP_ICON, [(1, group)]),
            (RT_VERSION, [(1, version_info(version))])]

    def dir_size(n):
        return 16 + 8 * n

    # Раскладка: корень, каталоги имён, каталоги языков, записи данных, сами данные.
    off = dir_size(len(tree))
    name_dirs = []
    for _, items in tree:
        name_dirs.append(off)
        off += dir_size(len(items))
    lang_dirs = []
    for _, items in tree:
        row = []
        for _ in items:
            row.append(off)
            off += dir_size(1)
        lang_dirs.append(row)
    entries = []
    for _, items in tree:
        row = []
        for _ in items:
            row.append(off)
            off += 16
        entries.append(row)
    datas = []
    for _, items in tree:
        row = []
        for _, data in items:
            off = (off + 7) & ~7
            row.append(off)
            off += len(data)
        datas.append(row)
    raw = bytearray(off)

    def put_dir(at, children):
        struct.pack_into('<IIHHHH', raw, at, 0, 0, 0, 0, 0, len(children))
        for k, (ident, target, is_dir) in enumerate(children):
            struct.pack_into('<II', raw, at + 16 + 8 * k, ident, target | (0x80000000 if is_dir else 0))

    put_dir(0, [(t, name_dirs[i], True) for i, (t, _) in enumerate(tree)])
    relocs = []
    for i, (_, items) in enumerate(tree):
        put_dir(name_dirs[i], [(ident, lang_dirs[i][j], True) for j, (ident, _) in enumerate(items)])
        for j, (_, data) in enumerate(items):
            put_dir(lang_dirs[i][j], [(LANG, entries[i][j], False)])
            # OffsetToData — RVA, исправляется линковщиком (ADDR32NB относительно символа .rsrc)
            struct.pack_into('<IIII', raw, entries[i][j], datas[i][j], len(data), 0, 0)
            relocs.append(entries[i][j])
            raw[datas[i][j]:datas[i][j] + len(data)] = data

    raw_ptr = 20 + 40
    reloc_ptr = raw_ptr + len(raw)
    sym_ptr = reloc_ptr + 10 * len(relocs)
    hdr = struct.pack('<HHIIIHH', 0x8664, 1, 0, sym_ptr, 2, 0, 0)
    sec = struct.pack('<8sIIIIIIHHI', b'.rsrc', 0, 0, len(raw), raw_ptr, reloc_ptr, 0, len(relocs), 0, 0x40000040)
    rel = b''.join(struct.pack('<IIH', r, 0, 0x0003) for r in relocs)  # IMAGE_REL_AMD64_ADDR32NB
    sym = struct.pack('<8sIhHBB', b'.rsrc', 0, 1, 0, 3, 1)
    aux = struct.pack('<IHHIHB3s', len(raw), len(relocs), 0, 0, 0, 0, b'')
    return hdr + sec + bytes(raw) + rel + sym + aux + struct.pack('<I', 4)


if __name__ == '__main__':
    version = cargo_version()
    obj = build(read_ico(ROOT / 'assets' / 'icon.ico'), version)
    (ROOT / 'assets' / 'icon.o').write_bytes(obj)
    print(f'assets/icon.o: {len(obj)} байт, версия {version}')
