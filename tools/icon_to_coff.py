"""Собирает assets/icon.o (COFF-объект с секцией .rsrc) из assets/icon.ico.

Так иконка попадает в .exe без windres/rc: build.rs передаёт icon.o линковщику.
Запуск после замены иконки:  python tools/icon_to_coff.py
"""
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RT_ICON, RT_GROUP_ICON, LANG = 3, 14, 0x0409


def read_ico(path):
    ico = path.read_bytes()
    _, _, count = struct.unpack_from('<HHH', ico, 0)
    out = []
    for i in range(count):
        w, h, cc, _, planes, bits, size, off = struct.unpack_from('<BBBBHHII', ico, 6 + 16 * i)
        out.append((w, h, cc, planes, bits, ico[off:off + size]))
    return out


def build(images):
    group = struct.pack('<HHH', 0, 1, len(images))
    for i, (w, h, cc, planes, bits, data) in enumerate(images, start=1):
        group += struct.pack('<BBBBHHIH', w, h, cc, 0, planes, bits, len(data), i)
    # тип -> [(id, данные)], типы и id по возрастанию
    tree = [(RT_ICON, [(i, img[5]) for i, img in enumerate(images, start=1)]),
            (RT_GROUP_ICON, [(1, group)])]

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
    obj = build(read_ico(ROOT / 'assets' / 'icon.ico'))
    (ROOT / 'assets' / 'icon.o').write_bytes(obj)
    print(f'assets/icon.o: {len(obj)} байт')
