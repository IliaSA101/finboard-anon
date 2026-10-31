"""Собирает THIRD_PARTY_NOTICES.txt: лицензии всех крейтов, попадающих в сборку под Windows.

Запуск из корня проекта: python tools/third_party.py
Список берётся из `cargo metadata`, тексты — из файлов LICENSE*/COPYING*/NOTICE* в исходниках крейтов
(одинаковые тексты печатаются один раз со списком крейтов). Шрифты egui лежат в epaint_default_fonts/fonts.
"""
import hashlib
import json
import os
import subprocess
import sys

TARGET = "x86_64-pc-windows-gnu"
OUT = "THIRD_PARTY_NOTICES.txt"
NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")


def license_files(folder):
    found = []
    for root, dirs, files in os.walk(folder):
        dirs[:] = [d for d in dirs if d in ("fonts", "LICENSES", "licenses")]
        for f in sorted(files):
            if f.upper().startswith(NAMES) or (root.endswith("fonts") and f.lower().endswith(".txt")):
                found.append(os.path.join(root, f))
    return found


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET], text=False))
    used = {n["id"] for n in meta["resolve"]["nodes"]}
    pkgs = sorted((p for p in meta["packages"] if p["id"] in used and p["source"]),
                  key=lambda p: (p["name"], p["version"]))

    texts = {}  # хэш текста -> (текст, [крейты])
    missing = []
    for p in pkgs:
        tag = f'{p["name"]} {p["version"]}'
        files = license_files(os.path.dirname(p["manifest_path"]))
        if not files:
            missing.append(tag)
        for path in files:
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read().replace("\r\n", "\n").strip()
            key = hashlib.sha256(text.encode()).hexdigest()
            texts.setdefault(key, (text, os.path.basename(path), []))[2].append(tag)

    out = [
        "finboard-anon использует сторонние компоненты. Ниже — их список и тексты лицензий.",
        "Сама программа распространяется по GNU GPL v3 (файл LICENSE).",
        "",
        "Сборка под Windows (тулчейн GNU) также включает среду выполнения MinGW-w64",
        "(libgcc — GPL с исключением GCC Runtime Library Exception, winpthreads — MIT/ZPL).",
        "",
        "=" * 78,
        "СПИСОК КОМПОНЕНТОВ (крейт, версия, лицензия)",
        "=" * 78,
    ]
    for p in pkgs:
        out.append(f'{p["name"]} {p["version"]} — {p["license"] or "см. файл лицензии"}'
                   + (f' — {p["repository"]}' if p.get("repository") else ""))
    for text, fname, tags in sorted(texts.values(), key=lambda t: t[2][0]):
        out += ["", "=" * 78, f"{fname}: " + ", ".join(tags), "=" * 78, "", text]
    if missing:
        out += ["", "Без отдельного файла лицензии (лицензия указана в списке выше): " + ", ".join(missing)]

    with open(OUT, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out) + "\n")
    print(f"{OUT}: крейтов {len(pkgs)}, разных текстов {len(texts)}, без файла {len(missing)}")


if __name__ == "__main__":
    sys.exit(main())
