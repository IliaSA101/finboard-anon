"""Мутационная проверка тестов: портит ключевое место кода и убеждается, что тесты падают.

Каждая мутация — (файл, что заменить, на что, какой баг она изображает). Файл всегда
восстанавливается. Мутант, которого тесты не убили, — дыра в тестах: нужен новый тест.
Мутанты проверяются параллельно в копиях проекта (target/mutate/wN), исходники проекта не трогаются —
их можно править во время прогона. Зависший прогон обрывается по таймауту и считается убитым.
Запуск:  python tools/mutate.py              (нужен cargo в PATH)
         python tools/mutate.py --only ИНН   (только мутанты с «ИНН» в описании или пути)
         python tools/mutate.py -j 2         (число одновременных проверок)
"""
import argparse
import os
import queue
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

MUTANTS = [
    ('src/anon.rs', 'if v.special || crate::detect::is_label(&v.text) {', 'if v.special {',
     'заголовки и «Итого» в диапазоне правила заменяются'),
    ('src/detect.rs', '    neighbors || text.split_whitespace().count() <= 2', '    true',
     'название раздела («Итоги по исполнителям») считается заголовком'),
    ('src/detect.rs', 'if !has_data || total || new_header {', 'if !has_data || new_header {',
     'таблица не останавливается на строке «Итого»'),
    ('src/detect.rs', 'let has_data = (c1..=c2).any(|x| grid.get(row, x).is_some());', 'let has_data = true;',
     'таблица продолжается через пустые строки в следующую'),
    ('src/anon.rs', 'profile.rules.clone())], &profile.settings, key_in)', 'profile.rules.clone())], &Settings::default(), key_in)',
     'предпросмотр со своей солью — показывает не то, что попадёт в файл'),
    ('src/anon.rs', '                if samples.len() < 3 && !samples.contains(&v.text) {', '                if false {',
     'у второго правила с теми же людьми нет примеров в предпросмотре'),
    ('src/fake.rs', '    let original = strip_id_suffix(original);', '',
     'повторное обезличивание «Имя (ID 3)» теряет пол и формат'),
    ('src/profile.rs', '                Some(_) if !replace => continue,', '                Some(_) => continue,',
     'повторное выделение не заменяет пересекающееся правило'),
    ('src/fake.rs', '        if let Some(&i) = self.cache.get(&key) {\n            return self.entries[i].fake.clone();\n        }', '',
     'один и тот же человек получает разные фейки'),
    ('src/anon.rs', '            excluded[m.pattern().as_usize()] = true;', '',
     'проверка утечек ругается на реальные имена внутри фейков (ложная тревога)'),
    ('src/anon.rs', '        for m in ac.find_iter(&data) {\n            count += 1;', '        for m in ac.find_iter(&data).take(0) {\n            count += 1;',
     'проверка утечек ничего не находит (пустой страж)'),
    ('src/anon.rs', '            let l = rule_hit(r);', '            let l = 0u8 + 0 * rule_hit(r);',
     'производные кэши формул (ЛЕВСИМВ(ФИО), =C2) не очищаются'),
    ('src/anon.rs', 'fn date_base(d1904: bool) -> i64 {\n    if d1904 {', 'fn date_base(d1904: bool) -> i64 {\n    if false {',
     'книга 1904: даты в кэше сводной не узнаются'),
    ('src/anon.rs', '        if idx.len() >= 2 {\n            let pieces', '        if false {\n            let pieces',
     'имя, разрезанное на фрагменты в примечании, не заменяется'),
    ('src/anon.rs', 'let mut v = vec![s.to_string(), text, attr, full];', 'let mut v = vec![s.to_string(), attr, full];',
     'проверка не видит текст с & и " вместе'),
    ('src/anon.rs', '} else if ln == b"tableColumn" && key == b"name" {', '} else if false {',
     'шапка умной таблицы расходится с tableColumn'),
    ('src/anon.rs', 'if let Some(rest) = target.strip_prefix("mailto:") {', 'if let Some(rest) = target.strip_prefix("nomailto:") {',
     'адрес в гиперссылке mailto: остаётся'),
    ('src/anon.rs', 'if v.special || crate::detect::is_label(&v.text) {', 'if crate::detect::is_label(&v.text) {',
     'ошибки и логические значения в колонке правила заменяются'),
    ('src/fake.rs', '                        self.prepare_people(std::iter::once(e.real.as_str()));', '',
     'после загрузки ключа инициалы не связываются с полным ФИО'),
    ('src/fake.rs', 'while self.used_person.get(&fake).is_some_and(|owner| *owner != id_norm) || taken(&fake, self) {', 'while false {',
     'без ID разные люди получают один фейк'),
    ('src/fake.rs', '                            self.used_company.insert(norm_text(&fname));', '',
     'организации из ключа совпадают с новыми'),
    ('src/fake.rs', '            let digits = ["ID", "Id", "iD", "id"].iter().find_map(|p| inner.strip_prefix(p)).map(str::trim);', '            let digits = if inner.len() > 2 && inner[..2].eq_ignore_ascii_case("id") { inner.get(2..).map(str::trim) } else { None };',
     'паника на «(№ 5)» (срез строки по байтам)'),
    ('src/detect.rs', '    while first <= r + 2 &&', '    while false && first <= r + 2 &&',
     'строка подзаголовков попадает в данные'),
    ('src/detect.rs', 'if is_label(below) || (kind_for_header(below).is_some() && neighbors) {', 'if false {',
     'групповая шапка «Клиент» над подзаголовками считается заголовком'),
    ('src/detect.rs', " || text.contains('@') ", ' ',
     'адрес @mail.ru принимается за заголовок «mail»'),
    ('src/detect.rs', ' || (r > 1 && grid.get(r - 1, x).is_some())', '',
     'двухстрочная шапка: таблица обрывается на клиенте без контактов'),
    ('src/anon.rs', '        return digits >= 9 && digits == k.trim().len();', '        return false;',
     'телефон-число внутри текста (примечание «тел. 9161234567») остаётся'),
    ('src/anon.rs', '&[(b"author", "Автор {n}")]', '&[(b"author", "Автор")]',
     'два автора примечаний становятся одинаковыми (порча файла)'),
    ('src/anon.rs', '} else if bound_hit && BOUNDS.contains', '} else if false && BOUNDS.contains',
     'в сводной снимается одна граница дат без другой (порча файла)'),
    ('src/anon.rs', 'clean_meta && ln == b"pivotCacheDefinition" && key == b"refreshedBy"', 'false && key == b"refreshedBy"',
     'имя обновлявшего сводную остаётся'),
    ('src/anon.rs', '        faker.set_taken(wb.sst.iter().map(String::as_str));\n', '',
     'фейк совпадает с уже имеющейся строкой книги (повтор колонок таблицы)'),
    ('src/anon.rs', '            None if level > 0 => {\n                w.write_event(Event::Empty(copy_without(start, b"t")))?;', '            None if false => {\n                w.write_event(Event::Empty(copy_without(start, b"t")))?;',
     'значения, разлитые формулой-массивом, остаются'),
    ('src/anon.rs', 'for (table, cols) in struct_refs(&text) {', 'for (table, cols) in struct_refs("") {',
     'производные через [@ФИО] не очищаются'),
    ('src/anon.rs', '(level >= 2 || maps.is_value_original(&v))', '(maps.is_value_original(&v))',
     'ГОД(дата рождения) остаётся в кэше'),
    ('src/anon.rs', '// Вне ячеек: колонтитулы, формулы условного форматирования и проверки данных (в т.ч. в extLst).\n            Event::Text(e) => {', '// x\n            Event::Text(e) if false => {',
     'колонтитулы, условное форматирование, проверка данных остаются'),
    ('src/anon.rs', 'b"hyperlink" | b"dataValidation") => {', 'b"__none__") => {',
     'подсказки гиперссылок и тексты проверки данных остаются'),
    ('src/anon.rs', '                    in_name = true;', '                    in_name = false;',
     'определённые имена со строками остаются'),
    ('src/anon.rs', '.filter(|n| *n != separately)', '.filter(|_| true)',
     'склейка фрагментов без нужды (обычный текст становится жирным)'),
    ('src/detect.rs', '(digits >= 5 && letters < 3)', 'digits >= 5',
     'шапка с датой («Сотрудники на 01.10.2024») не узнаётся'),
    ('src/detect.rs', 'if is_label(below) || (kind_for_header(below).is_some() && neighbors) {', 'if is_label(below) || kind_for_header(below).is_some() {',
     'первое значение («Компания Вектор») считается подзаголовком'),
    ('src/detect.rs', '            if out.iter().any(|x| x.covers(r, c)) {', '            if false {',
     'значение внутри найденной таблицы считается новым заголовком'),
    ('src/anon.rs', '    let total = (v * 86400.0).round() as i64;\n    let (y, m, d) = crate::fake::civil_from_days(date_base(d1904) + total.div_euclid(86400));\n    let secs = total.rem_euclid(86400);', '    let (y, m, d) = crate::fake::civil_from_days(date_base(d1904) + v.trunc() as i64);\n    let secs = (v.fract() * 86400.0).round() as i64;',
     'время около полуночи превращается в «T24:00:00»'),

    # --- несколько файлов, организации, ИНН
    ('src/fake.rs', '        let key = norm_text(&name);\n        let s = if is_ip', '        let key = norm_text(orig);\n        let s = if is_ip',
     'организация в разных написаниях («ООО «Ромашка»», «Ромашка») получает разные фейки'),
    ('src/fake.rs', "for (a, b) in [('«', '»'), ('\"', '\"'), ('“', '”'), ('„', '“')] {", "for (a, b) in [('«', '»')] {",
     'название в обычных кавычках («ООО \"Ромашка\"») не узнаётся'),
    ('src/fake.rs', '    if toks.len() > 1 && is_form(toks[toks.len() - 1]) {', '    if false {',
     'форма в конце («Вектор АО») не отделяется от названия'),
    ('src/fake.rs', '            if let Some(fd) = self.digits.get(&d).cloned() {', '            if let Some(fd) = None::<String> {',
     'один ИНН в разном виде («77 07 083893», «ИНН: …») получает разные фейки'),
    ('src/fake.rs', '                            self.digits.entry(d).or_insert(fd);', '',
     'ИНН без ID из ключа не восстанавливается — в следующем файле другой фейк'),
    ('src/fake.rs', '                            self.company.entry(norm_text(&rn)).or_insert(base);', '                            let _ = base;',
     'организация из ключа в другом написании получает новый фейк'),
    ('src/fake.rs', '        let s = if is_ip(&pre, &post) {', '        let s = if false {',
     'ИП-контрагент и тот же человек в колонке ФИО — разные фейки'),
    ('src/fake.rs', '                            self.register_person(&rn, &fname);', '',
     'ИП из ключа и тот же человек в другом файле — разные фейки'),
    ('src/fake.rs', '            tail.replace_range(0..1, "9");', '',
     'номер счёта-число превращается в текст (ведущий ноль)'),
    ('src/fake.rs', 'format!("9900{body}")', 'format!("0000{body}")',
     'ИНН-число превращается в текст (ведущие нули), ВПР по ИНН ломается'),
    ('src/anon.rs', '    for e in &faker.entries {\n        maps.strings.entry', '    for e in faker.entries.iter().take(0) {\n        maps.strings.entry',
     'значение из другого файла (ИНН в колонке без правила) не заменяется'),
    ('src/anon.rs', '        maps.extra_inner.push((real.to_string(), fake.to_string()));', '',
     'организация внутри текста («Оплата ООО \"Ромашка\"») не заменяется'),
    ('src/anon.rs', '    if !has_f && (t.is_empty() || t == "n") && v.len() >= 9 {', '    if false {',
     'длинное число (ИНН, телефон) в колонке без правила остаётся'),
    ('src/anon.rs', '    for (_, wb) in books.iter() {', '    for (_, wb) in books.iter().take(1) {',
     'фейк совпадает с реальным значением из другого файла пакета'),
    ('src/anon.rs', 'faker.prepare_people(all().filter', 'faker.prepare_people(all().take(0).filter',
     '«Иванов И.И.» в одном файле не связывается с полным ФИО из другого'),
    ('src/anon.rs', '.filter(|r| !own[fi].contains(r) && inner_candidate(r))', '.filter(|_| false)',
     'проверка не видит в файле значения других файлов и ключа'),
    ('src/anon.rs', '        if k.exists() && !batch.key_in', '        if false && !batch.key_in',
     'прогон без загрузки ключа затирает общий ключ папки'),
    ('src/anon.rs', '            while !used.insert(out.to_string_lossy().to_lowercase()) {', '            while false {',
     'одноимённые файлы из разных папок затирают друг друга в папке результатов'),
    ('src/anon.rs', '[shared_key_path(input), legacy_key_path(input)]', '[legacy_key_path(input), shared_key_path(input)]',
     'старый ключ файла важнее общего ключа папки'),
    ('src/bin/cli.rs', '    let key_in = load_key.or(found);', '    let key_in = load_key;',
     'консоль затирает общий ключ папки вместо дополнения'),
    ('src/detect.rs', '"плательщик", "покупател",', '"покупател",',
     'шапка «Плательщик» не узнаётся как контрагент'),

    # --- любые связки: одно значение в любом виде — один фейк
    ('src/fake.rs', '        if let Some(fk) = self.phones.get(&key).cloned() {', '        if let Some(fk) = None::<String> {',
     'телефон «8 916…» и «+7 (916)…» без ID получают разные фейки'),
    ('src/fake.rs', '        if let Some(e) = self.emails.get(&norm).cloned() {', '        if let Some(e) = None::<String> {',
     'email в другом регистре в другом файле получает другой фейк'),
    ('src/fake.rs', '        let s = match self.addresses.get(&norm) {', '        let s = match None::<&String> {',
     'адрес с другими пробелами/регистром получает другой фейк'),
    ('src/fake.rs', '        let offset = match day.and_then(|d| self.date_shift.get(&d)) {', '        let offset = match None::<&i64> {',
     'дата числом и та же дата текстом сдвигаются по-разному'),
    ('src/fake.rs', '                                self.date_shift.entry(r).or_insert(f - r);', '',
     'сдвиг даты из ключа не восстанавливается'),
    ('src/fake.rs', '                                    self.emails.entry(r.to_lowercase()).or_insert_with(|| f.clone());', '',
     'email из ключа в другом регистре — новый фейк'),
    ('src/fake.rs', '                                        self.phones.entry(phone_key(&dr)).or_insert_with(|| phone_key(&df));', '',
     'телефон из ключа в другом формате — новый фейк'),
    ('src/fake.rs', '                        self.addresses.entry(norm_text(strip_id_suffix(&e.real))).or_insert(fake_core);', '                        let _ = fake_core;',
     'адрес из ключа в другом написании — новый фейк'),
    ('src/fake.rs', '        if self.known_fakes.contains(original) {', '        if false {',
     'повторный прогон по результату с ключом обезличивает фейки второй раз'),
    ('src/fake.rs', '                self.company_text.entry(name.trim().to_string()).or_insert_with(|| p.clone());', '',
     'ФИО ИП в колонке без правила и в тексте остаётся'),
    ('src/fake.rs', 'matches!(t.len(), 9 | 11)', 'matches!(t.len(), 99)',
     'ИНН числом без ведущего нуля и тот же ИНН текстом — разные фейки'),
    ('src/fake.rs', '                            let base = if fname.chars().any(char::is_lowercase) { fname.clone() } else { title_case(&fname) };', '                            let base = fname.clone();',
     'фейк из ключа остаётся ЗАГЛАВНЫМИ для обычного написания'),
    ('src/anon.rs', '    kind_conflicts(&mut plans, rules, &faker, files);', '',
     'одни значения под разными типами — без предупреждения'),
    ('src/anon.rs', '            let whole = self.lookup_digits(&digits_only(&s[a..b]));', '            let whole: Option<String> = None;',
     'ИНН с пробелами («77 07 083893») вне правил не заменяется'),
    ('src/anon.rs', '        .flat_map(|(k, v)| [(k.to_uppercase(), v.to_uppercase()), (k.to_lowercase(), v.to_lowercase())])', '        .flat_map(|(k, v)| [(k.clone(), v.clone())])',
     'ФИО и организации ЗАГЛАВНЫМИ вне правил не заменяются'),
    ('src/anon.rs', '        if !digits.is_empty() {\n            let text', '        if false {\n            let text',
     'проверка не видит номер, записанный иначе («77 07 083893»)'),
    ('src/anon.rs', '            cased.push(s.to_uppercase());', '',
     'проверка не видит ФИО и организации ЗАГЛАВНЫМИ'),
    ('src/anon.rs', "            if rest.starts_with('!') || rest.starts_with(\"'!\") {", '            if false {',
     'ссылки на лист с именем организации переписываются (ссылки в никуда)'),
    ('src/anon.rs', '            if !dir.is_dir() {', '            if false {',
     'сбой на середине пакета: часть файлов записана без ключа'),
    ('src/anon.rs', '            .or_else(|| v.bytes().all(|c| c.is_ascii_digit()).then(|| maps.lookup_digits(&v)).flatten());', '            .or_else(|| None::<String>);',
     'ИНН числом без ведущего нуля в колонке без правила остаётся'),
    ('src/anon.rs', '        } else if (t.is_empty() || t == "n") && maps.is_known_number(&v) {', '        } else if false {',
     'ИНН в кэше формулы (ВПР из справочника) остаётся'),
    ('src/anon.rs', '        Some(k) => shared_key_path(k),', '        Some(_) => shared_key_path(first_input),',
     'ключ второй папки устаревает: новые записи уходят в ключ первой'),
    ('src/bin/cli.rs', '    let found = find_key(&first);', '    let found = Some(shared_key_path(&first)).filter(|k| k.exists());',
     'консоль не подхватывает старый ключ файла'),
]


COPY = ['src', 'tests', 'assets', 'samples', 'build.rs', 'Cargo.toml', 'Cargo.lock']
TIMEOUT = 300  # обычный прогон ~20 с; дольше — мутант зациклил или замедлил код


def cargo_test(cwd):
    """cargo test без --no-fail-fast: стоп на первом упавшем тестовом файле. (код, вывод, зависло?)"""
    p = subprocess.Popen(['cargo', 'test'], cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         text=True, encoding='utf-8', errors='replace')
    try:
        out, err = p.communicate(timeout=TIMEOUT)
        return p.returncode, out, err, False
    except subprocess.TimeoutExpired:
        subprocess.run(['taskkill', '/PID', str(p.pid), '/T', '/F'], capture_output=True)
        p.communicate()
        return 1, '', '', True


def make_worker(n):
    """Копия проекта в target/mutate/wN со своей сборкой: мутанты не трогают исходники проекта."""
    w = ROOT / 'target' / 'mutate' / f'w{n}'
    for name in COPY:
        s, d = ROOT / name, w / name
        if d.exists():
            shutil.rmtree(d) if d.is_dir() else d.unlink()
        # Копировать с текущей датой (не copy2): иначе сборка в копии от прошлого мутанта новее файлов,
        # cargo не пересобирает и тесты идут на испорченном коде.
        if s.is_dir():
            shutil.copytree(s, d, copy_function=shutil.copy)
        elif s.exists():
            d.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(s, d)
    if not (w / 'target').exists() and (ROOT / 'target' / 'debug').exists():
        shutil.copytree(ROOT / 'target' / 'debug', w / 'target' / 'debug',
                        ignore=shutil.ignore_patterns('incremental', 'mutate'))
    code, out, err, hung = cargo_test(w)
    if code != 0:
        sys.exit(f'Исходный код не проходит тесты в копии {w} — мутации бессмысленны.\n{out[-2000:]}{err[-2000:]}')
    return w


def run_mutant(w, mutant):
    path, old, new, what = mutant
    f = w / path
    src = f.read_text(encoding='utf-8')
    if src.count(old) != 1:
        return '??', f'место для мутации не найдено ({path})'
    f.write_text(src.replace(old, new), encoding='utf-8')
    try:
        code, out, err, hung = cargo_test(w)
    finally:
        f.write_text(src, encoding='utf-8')
    if hung:
        return 'убит', f'тесты не уложились в {TIMEOUT} с'
    if code == 0:
        return 'ЖИВ', ''
    failed = [l.split()[1] for l in out.splitlines() if l.startswith('test ') and l.endswith('FAILED')]
    return 'убит', ', '.join(failed) if failed else 'не компилируется' if 'could not compile' in err else 'прогон упал'


def main():
    sys.stdout.reconfigure(encoding='utf-8', line_buffering=True)
    ap = argparse.ArgumentParser(description='Мутационная проверка тестов')
    ap.add_argument('-j', '--jobs', type=int, default=max(1, min(4, (os.cpu_count() or 2) // 3)),
                    help='сколько мутантов проверять одновременно (каждый в своей копии проекта)')
    ap.add_argument('--only', action='append', default=[],
                    help='только мутанты, у которых путь или описание содержит этот текст (можно несколько)')
    args = ap.parse_args()
    todo = [m for m in MUTANTS if not args.only or any(t in m[0] or t in m[3] for t in args.only)]
    jobs = max(1, min(args.jobs, len(todo)))
    print(f'Мутантов: {len(todo)} из {len(MUTANTS)}, потоков: {jobs}. Готовлю копии проекта…')
    start = time.time()
    with ThreadPoolExecutor(jobs) as ex:
        workers = queue.Queue()
        for w in ex.map(make_worker, range(1, jobs + 1)):
            workers.put(w)
        print(f'Копии готовы за {time.time() - start:.0f} с')

        def task(m):
            w = workers.get()
            try:
                return m, run_mutant(w, m)
            finally:
                workers.put(w)

        survived = []
        for i, fut in enumerate(as_completed([ex.submit(task, m) for m in todo]), 1):
            m, (verdict, reason) = fut.result()
            if verdict != 'убит':
                survived.append(m[3])
            print(f'[{i}/{len(todo)}] {verdict:4}  {m[3]}' + (f'  ← {reason}' if reason else ''))
    print(f'\nМутантов: {len(todo)}, выжило: {len(survived)}, время: {(time.time() - start) / 60:.1f} мин')
    sys.exit(1 if survived else 0)


if __name__ == '__main__':
    main()
