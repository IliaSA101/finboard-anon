//! Обезличивание книги: сбор значений по правилам, генерация фейков, перезапись частей zip,
//! проверка результата на остатки реальных данных.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use aho_corasick::{AhoCorasick, MatchKind};
use anyhow::{bail, Context, Result};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::fake::Faker;
use crate::profile::{parse_kind_key, Kind, Profile, Rule, Settings};
use crate::xlsx::{attr, open_zip, parse_range, pivot_caches, read_part, resolve_value, scan_cells, PivotCache, Workbook, Zip};
use crate::keyfile;

pub struct Job {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Ключ прошлого запуска — чтобы сохранить те же ID.
    pub key_in: Option<PathBuf>,
    /// Куда записать ключ этого запуска.
    pub key_out: Option<PathBuf>,
    pub profile: Profile,
}

#[derive(Clone, Debug)]
pub struct Leak {
    pub part: String,
    pub count: usize,
    pub samples: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub output: PathBuf,
    pub key_path: Option<PathBuf>,
    /// Заменено ячеек по каждому правилу.
    pub rule_cells: Vec<usize>,
    pub distinct_values: usize,
    pub sst_replaced: usize,
    pub formula_cache_replaced: usize,
    pub formula_text_replaced: usize,
    pub formula_cache_cleared: usize,
    pub other_replaced: usize,
    pub parts_changed: usize,
    pub warnings: Vec<String>,
    pub leaks: Vec<Leak>,
}

/// Общий ключ папки: все файлы одной папки получают одни и те же фейки для одних и тех же людей,
/// организаций, ИНН. Имя не даст случайно переслать его вместе с файлами.
pub fn shared_key_path(input: &Path) -> PathBuf {
    input.with_file_name("КЛЮЧ_ОБЕЗЛИЧИВАНИЯ_НЕ_ПЕРЕДАВАТЬ.csv")
}

/// Ключ прежних версий — свой у каждого файла.
fn legacy_key_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("книга");
    input.with_file_name(format!("{stem}_КЛЮЧ_НЕ_ПЕРЕДАВАТЬ.csv"))
}

/// Куда писать общий ключ. Выбранный вручную — в него же; найденный при открытии (общий ключ папки или
/// старый ключ файла) — в общий ключ той папки; иначе — общий ключ папки первого файла.
pub fn key_out_path(first_input: &Path, key_in: Option<&Path>, chosen: bool) -> PathBuf {
    match key_in {
        Some(k) if chosen => k.to_path_buf(),
        Some(k) => shared_key_path(k),
        None => shared_key_path(first_input),
    }
}

/// Ключ, который подхватывается при открытии файла: общий ключ папки, иначе старый ключ этого файла.
pub fn find_key(input: &Path) -> Option<PathBuf> {
    [shared_key_path(input), legacy_key_path(input)].into_iter().find(|p| p.exists())
}

/// Имя результата по умолчанию: «<имя>_anon.xlsx».
pub fn default_output_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("книга");
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("xlsx");
    input.with_file_name(format!("{stem}_anon.{ext}"))
}

/// Результаты пакета: «<имя>_anon.xlsx» рядом с исходным или в выбранной папке.
/// Одинаковые имена из разных папок не затирают друг друга: «отчет_anon_2.xlsx».
pub fn batch_outputs(inputs: &[PathBuf], dir: Option<&Path>) -> Vec<PathBuf> {
    let mut used: HashSet<String> = HashSet::new();
    inputs
        .iter()
        .map(|input| {
            let base = default_output_path(input);
            let base = match dir {
                Some(d) => d.join(base.file_name().unwrap_or_default()),
                None => base,
            };
            let mut out = base.clone();
            let mut n = 2;
            while !used.insert(out.to_string_lossy().to_lowercase()) {
                let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("книга");
                let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("xlsx");
                out = base.with_file_name(format!("{stem}_{n}.{ext}"));
                n += 1;
            }
            out
        })
        .collect()
}

impl Report {
    pub fn summary(&self, profile: &Profile) -> Vec<String> {
        let mut l = vec![format!("Готово: {}", self.output.display())];
        for (i, r) in profile.rules.iter().enumerate() {
            let n = self.rule_cells.get(i).copied().unwrap_or(0);
            l.push(format!("  {}!{} [{}]: заменено ячеек {}", r.sheet, r.range_text(), r.kind.title(), n));
        }
        l.push(format!(
            "Уникальных значений: {}; общих строк: {}; формул (текст/кэш/очищено кэшей): {}/{}/{}; прочих мест (сводные, диаграммы, примечания): {}",
            self.distinct_values, self.sst_replaced, self.formula_text_replaced, self.formula_cache_replaced, self.formula_cache_cleared, self.other_replaced
        ));
        if let Some(k) = &self.key_path {
            l.push(format!("Ключ (НЕ передавать вместе с файлом): {}", k.display()));
        }
        for w in &self.warnings {
            l.push(format!("Внимание: {w}"));
        }
        if self.leaks.is_empty() {
            l.push("Проверка: реальных значений из правил в файле не найдено.".into());
        } else {
            l.push("Проверка: реальные значения ещё встречаются:".into());
            for k in &self.leaks {
                l.push(format!("  {} — {} вхожд., например: {}", k.part, k.count, k.samples.join(" | ")));
            }
            l.push("  Обычно это текст формул, заголовки, имена листов или колонки, не попавшие в правила.".into());
        }
        l
    }
}

/// Значения, которые заменяются везде, где встречаются целиком: общие строки, кэш формул,
/// сводные таблицы, диаграммы, примечания.
#[derive(Default)]
pub struct GlobalMaps {
    strings: HashMap<String, String>,
    numbers: HashMap<String, String>,
    /// Поиск реальных значений внутри длинного текста («Ответственный: Иванов Иван Иванович»).
    inner: Option<(AhoCorasick, Vec<String>)>,
    /// Пары для поиска внутри текста помимо длинных значений: названия организаций («Ромашка»),
    /// варианты ЗАГЛАВНЫМИ и строчными (банковские выписки).
    extra_inner: Vec<(String, String)>,
    /// Цифры номеров (от 9) и телефонов (10 цифр номера): реальные → фейковые. Номер узнаётся в любом
    /// написании: «7707083893», «77 07 083893», «+7 (916) 123-45-67».
    digits: HashMap<String, String>,
    phones: HashMap<String, String>,
    /// Искать номера внутри текста (настройка «искать внутри текста»).
    inner_digits: bool,
}

/// Номера в тексте: цифры, между группами — до двух знаков из « -()»: «77 07 083893», «+7 (916) 123-45-67».
fn digit_runs(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        let end = loop {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let mut k = j;
            while k < b.len() && k - j < 2 && b"-() ".contains(&b[k]) {
                k += 1;
            }
            if k > j && k < b.len() && b[k].is_ascii_digit() {
                j = k;
            } else {
                break j;
            }
        };
        out.push((start, end));
        i = end;
    }
    out
}

/// Группы цифр без разделителей внутри номера.
fn digit_groups(s: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        if b[i].is_ascii_digit() {
            let a = i;
            while i < end && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push((a, i));
        } else {
            i += 1;
        }
    }
    out
}

fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Подставляет цифры фейка на места цифр номера; если длина разная (ИНН без ведущего нуля) — номер целиком.
fn put_digits(orig: &str, fake: &str) -> String {
    if digits_only(orig).len() != fake.len() {
        return fake.to_string();
    }
    let mut it = fake.chars();
    orig.chars().map(|c| if c.is_ascii_digit() { it.next().unwrap_or('0') } else { c }).collect()
}

/// Внутри текста ищем только «длинные» значения: несколько слов, email, номера, числа от 9 цифр
/// (телефон или ИНН, хранящиеся числом: «тел. 9161234567» в примечании).
/// Одиночная фамилия может оказаться частью другого слова — её не ищем.
fn inner_candidate(k: &str) -> bool {
    let digits = k.chars().filter(|c| c.is_ascii_digit()).count();
    if k.trim().parse::<f64>().is_ok() {
        return digits >= 9 && digits == k.trim().len();
    }
    k.chars().count() >= 8 && (k.trim().contains(char::is_whitespace) || k.contains('@') || digits >= 7)
}

impl GlobalMaps {
    /// Фейковые цифры для номера: ИНН/СНИЛС/паспорт/счёт по цифрам, телефон — по 10 цифрам номера.
    fn lookup_digits(&self, d: &str) -> Option<String> {
        if d.len() >= 9 {
            if let Some(f) = self.digits.get(d) {
                return Some(f.clone());
            }
        }
        match d.len() {
            10 => self.phones.get(d).cloned(),
            11 if d.starts_with('7') || d.starts_with('8') => self.phones.get(&d[1..]).map(|f| format!("{}{f}", &d[..1])),
            _ => None,
        }
    }

    /// Номера внутри текста в любом написании.
    fn replace_digits(&self, s: &str) -> Option<String> {
        if self.digits.is_empty() && self.phones.is_empty() {
            return None;
        }
        let mut out = String::new();
        let mut last = 0;
        for (a, b) in digit_runs(s) {
            let whole = self.lookup_digits(&digits_only(&s[a..b]));
            let parts: Vec<(usize, usize, String)> = match whole {
                Some(f) => vec![(a, b, f)],
                None => digit_groups(s, a, b).into_iter().filter_map(|(x, y)| self.lookup_digits(&s[x..y]).map(|f| (x, y, f))).collect(),
            };
            for (x, y, f) in parts {
                out.push_str(&s[last..x]);
                out.push_str(&put_digits(&s[x..y], &f));
                last = y;
            }
        }
        if last == 0 {
            return None;
        }
        out.push_str(&s[last..]);
        Some(out)
    }

    fn build_inner(&mut self) {
        self.inner_digits = true;
        let mut pairs: Vec<(&String, &String)> = self.strings.iter().filter(|(k, v)| inner_candidate(k) && k != v).collect();
        pairs.extend(self.extra_inner.iter().filter(|(k, v)| k != v).map(|(k, v)| (k, v)));
        pairs.sort();
        pairs.dedup_by(|a, b| a.0 == b.0);
        if pairs.is_empty() {
            return;
        }
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(pairs.iter().map(|(k, _)| k.as_str()));
        if let Ok(ac) = ac {
            let outs = pairs.iter().map(|(_, v)| (*v).clone()).collect();
            self.inner = Some((ac, outs));
        }
    }

    /// Точное совпадение, а если его нет — замена вхождений внутри текста по границам слов.
    fn replace_in(&self, s: &str) -> Option<String> {
        if let Some(v) = self.any(s) {
            return Some(v.clone());
        }
        let words = self.replace_words(s);
        if !self.inner_digits {
            return words;
        }
        match self.replace_digits(words.as_deref().unwrap_or(s)) {
            Some(x) => Some(x),
            None => words,
        }
    }

    fn replace_words(&self, s: &str) -> Option<String> {
        let (ac, outs) = self.inner.as_ref()?;
        let boundary = |c: Option<char>| c.map_or(true, |c| !c.is_alphanumeric());
        let mut out = String::new();
        let mut last = 0;
        for m in ac.find_iter(s) {
            if !(boundary(s[..m.start()].chars().next_back()) && boundary(s[m.end()..].chars().next())) {
                continue;
            }
            // Ссылка на лист с таким именем ('Ромашка'!A1, Ромашка!A1): имя листа не меняется — и ссылка тоже.
            let rest = &s[m.end()..];
            if rest.starts_with('!') || rest.starts_with("'!") {
                continue;
            }
            out.push_str(&s[last..m.start()]);
            out.push_str(&outs[m.pattern().as_usize()]);
            last = m.end();
        }
        if last == 0 {
            return None;
        }
        out.push_str(&s[last..]);
        Some(out)
    }

    /// Для общих строк — любое значение из правил.
    fn sst(&self, s: &str) -> Option<&String> {
        self.strings.get(s)
    }

    /// Для прочих мест — только «надёжные» значения, чтобы не задеть случайные совпадения
    /// вроде «12» или «Да».
    fn any(&self, s: &str) -> Option<&String> {
        let numeric = s.trim().parse::<f64>().is_ok();
        if !numeric && s.chars().count() >= 3 {
            return self.strings.get(s);
        }
        // Числа (телефон, ИНН, счёт) — только длинные, где случайное совпадение невероятно.
        if numeric && s.len() >= 9 {
            return self.numbers.get(s).or_else(|| self.strings.get(s));
        }
        None
    }

    /// Текст ячейки после замены — так же, как в общих строках (для шапок умных таблиц и ссылок на них).
    fn cell_text(&self, s: &str) -> Option<String> {
        self.sst(s).cloned().or_else(|| self.replace_in(s))
    }

    fn is_value_original(&self, s: &str) -> bool {
        self.numbers.contains_key(s) || self.strings.contains_key(s)
    }

    fn is_known_number(&self, s: &str) -> bool {
        s.len() >= 9 && (self.numbers.contains_key(s) || self.strings.contains_key(s))
    }
}

#[derive(Default)]
struct Stats {
    rule_cells: Vec<usize>,
    sst: usize,
    formula: usize,
    formula_text: usize,
    formula_cleared: usize,
    other: usize,
}

pub fn run(job: &Job, log: &mut dyn FnMut(String)) -> Result<Report> {
    let batch = Batch {
        files: vec![FileJob { input: job.input.clone(), output: job.output.clone(), rules: job.profile.rules.clone() }],
        settings: job.profile.settings.clone(),
        key_in: job.key_in.clone(),
        key_out: job.key_out.clone(),
    };
    Ok(run_batch(&batch, log)?.remove(0))
}

/// Файл пакета: свои правила, а генератор фейков и ключ — общие для всех файлов.
#[derive(Clone, Debug)]
pub struct FileJob {
    pub input: PathBuf,
    pub output: PathBuf,
    pub rules: Vec<Rule>,
}

/// Несколько файлов одного заказчика за один прогон: один человек, одна организация, один ИНН —
/// один фейк во всех файлах. Значения из любого файла заменяются и в остальных — даже там,
/// где их колонка не попала в правила (ИНН контрагента в файле платежей).
#[derive(Clone, Debug)]
pub struct Batch {
    pub files: Vec<FileJob>,
    pub settings: Settings,
    /// Ключ прошлых запусков — чтобы сохранить те же ID и фейки.
    pub key_in: Option<PathBuf>,
    /// Куда записать общий ключ (в нём и загруженные записи).
    pub key_out: Option<PathBuf>,
}

pub fn run_batch(batch: &Batch, log: &mut dyn FnMut(String)) -> Result<Vec<Report>> {
    if batch.files.is_empty() {
        bail!("Нет файлов для обработки");
    }
    for (i, f) in batch.files.iter().enumerate() {
        let name = file_name(&f.input);
        if batch.files.iter().any(|g| same_file(&g.input, &f.output)) {
            bail!("{name}: файл результата совпадает с исходным — выберите другое имя");
        }
        if batch.files[..i].iter().any(|g| same_file(&g.output, &f.output)) {
            bail!("{name}: два файла пакета пишутся в один результат {}", f.output.display());
        }
        if f.rules.is_empty() {
            bail!("{name}: нет правил замены");
        }
    }
    let mut books = Vec::new();
    for f in &batch.files {
        let mut zip = open_zip(&f.input)?;
        let wb = Workbook::from_zip(&mut zip).with_context(|| file_name(&f.input))?;
        books.push((zip, wb));
    }
    let rules: Vec<&[Rule]> = batch.files.iter().map(|f| f.rules.as_slice()).collect();

    // 1–2. Значения под правилами всех файлов и их фейки — тем же кодом, что и предпросмотр.
    log("Сбор значений по правилам…".into());
    let paths: Vec<&Path> = batch.files.iter().map(|f| f.input.as_path()).collect();
    let (plans, faker, key_entries) = plan_batch(&mut books, &paths, &rules, &batch.settings, batch.key_in.as_deref())?;
    if batch.key_in.is_some() {
        log(format!("Загружен ключ: {key_entries} записей"));
    }
    log(format!("Уникальных значений: {}", faker.entries.len() - key_entries));

    // Папки результатов — до записи: сбой на середине пакета оставил бы часть файлов без ключа.
    for f in &batch.files {
        if let Some(dir) = f.output.parent().filter(|d| !d.as_os_str().is_empty()) {
            if !dir.is_dir() {
                bail!("{}: нет папки для результата {}", file_name(&f.input), dir.display());
            }
        }
    }

    // 3. Ключ — один на все файлы, до записи файлов: любой записанный результат покрыт ключом.
    // Чужой ключ на этом месте (прогон без загрузки ключа) не затирается: в нём соответствия уже
    // переданных файлов — он переименовывается.
    if let Some(k) = &batch.key_out {
        if k.exists() && !batch.key_in.as_deref().is_some_and(|i| same_file(i, k)) {
            let backup = keyfile::backup_path(k);
            std::fs::rename(k, &backup).with_context(|| format!("не переименовывается прежний ключ {}", k.display()))?;
            log(format!("Прежний ключ не был загружен и сохранён как {}", backup.display()));
        }
        keyfile::write(k, &faker.entries)?;
        log(format!("Ключ записан: {}", k.display()));
    }

    let mut reports = Vec::new();
    let mut scans = Vec::new();
    for (fi, f) in batch.files.iter().enumerate() {
        if batch.files.len() > 1 {
            log(format!("Файл {} из {}: {}", fi + 1, batch.files.len(), file_name(&f.input)));
        }
        let (zip, wb) = &mut books[fi];
        let (maps, scan_extra) = build_maps(&faker, &plans, &rules, wb.date1904, batch.settings.inner_text);
        let mut report = write_book(zip, wb, &f.rules, &faker, &maps, &batch.settings, &f.output, log)?;
        report.warnings.splice(0..0, plans[fi].warnings.iter().cloned());
        report.distinct_values = plans[fi].items.len();
        reports.push(report);
        scans.push(scan_extra);
    }

    if let Some(k) = &batch.key_out {
        for r in &mut reports {
            r.key_path = Some(k.clone());
        }
    }

    // 5. Проверка: свои значения файла — все; значения других файлов и ключа — те, что ищутся и внутри текста.
    log("Проверка результата на остатки реальных значений…".into());
    let mut fakes: Vec<&str> = faker.entries.iter().map(|e| e.fake.as_str()).collect();
    fakes.extend(faker.company_names().map(|(_, f)| f));
    // Номера по цифрам — в любом написании («77 07 083893»); фейковые цифры — не утечка.
    let fake_digits: HashSet<&str> = faker.digit_pairs().map(|(_, f)| f).chain(faker.phone_pairs().map(|(_, f)| f)).collect();
    let real_digits: HashSet<String> = faker
        .digit_pairs()
        .flat_map(|(r, _)| [Some(r.to_string()), r.strip_prefix('0').filter(|x| x.len() >= 9).map(String::from)])
        .flatten()
        .chain(faker.phone_pairs().filter(|(r, _)| r.len() == 10).map(|(r, _)| r.to_string()))
        .filter(|r| !fake_digits.contains(r.as_str()))
        .collect();
    let own: Vec<HashSet<&str>> = plans.iter().map(|p| p.items.iter().map(|(_, s, _)| s.as_str()).collect()).collect();
    for (fi, f) in batch.files.iter().enumerate() {
        let mut originals: Vec<String> = plans[fi].items.iter().map(|(_, s, _)| s.clone()).collect();
        originals.extend(scans[fi].iter().cloned());
        originals.extend(
            faker.entries.iter().map(|e| e.real.as_str()).filter(|r| !own[fi].contains(r) && inner_candidate(r)).map(String::from),
        );
        originals.extend(faker.company_names().map(|(r, _)| r.to_string()));
        originals.sort();
        originals.dedup();
        reports[fi].leaks = scan_leaks_with(&f.output, &originals, &fakes, &real_digits)?;
    }
    Ok(reports)
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Замены для мест вне правил: сначала значения этого прогона (всех файлов пакета), затем — из ключа.
/// Второе — даты в виде ISO для проверки (так их хранят сводные).
fn build_maps(faker: &Faker, plans: &[FilePlan], rules: &[&[Rule]], date1904: bool, inner: bool) -> (GlobalMaps, Vec<String>) {
    let mut maps = GlobalMaps::default();
    let mut scan_extra = Vec::new();
    let mut add_date = |maps: &mut GlobalMaps, original: &str, out: &str| {
        if let (Some(a), Some(b)) = (serial_to_iso(original, date1904), serial_to_iso(out, date1904)) {
            maps.strings.entry(a.clone()).or_insert(b);
            scan_extra.push(a);
        }
    };
    for (fi, p) in plans.iter().enumerate() {
        for (ri, original, numeric) in &p.items {
            let rule = &rules[fi][*ri];
            let out = faker.get(&rule.kind_key(), original).unwrap_or_default().to_string();
            maps.strings.entry(original.clone()).or_insert_with(|| out.clone());
            if *numeric {
                maps.numbers.entry(original.clone()).or_insert_with(|| out.clone());
                // Сводные таблицы хранят даты как «1982-02-16T00:00:00».
                if rule.kind == Kind::BirthDate {
                    add_date(&mut maps, original, &out);
                }
            }
        }
    }
    for e in &faker.entries {
        maps.strings.entry(e.real.clone()).or_insert_with(|| e.fake.clone());
        if is_plain_number(&e.real) && is_plain_number(&e.fake) {
            maps.numbers.entry(e.real.clone()).or_insert_with(|| e.fake.clone());
            if parse_kind_key(&e.kind_key).is_some_and(|(k, _, _)| k == Kind::BirthDate) {
                add_date(&mut maps, &e.real, &e.fake);
            }
        }
    }
    for (real, fake) in faker.company_names() {
        maps.strings.entry(real.to_string()).or_insert_with(|| fake.to_string());
        maps.extra_inner.push((real.to_string(), fake.to_string()));
    }
    // Тот же текст ЗАГЛАВНЫМИ или строчными («ООО "РОМАШКА"», «ИВАНОВ ИВАН ИВАНОВИЧ» в выписке банка).
    let variants: Vec<(String, String)> = maps
        .strings
        .iter()
        .filter(|(k, v)| inner_candidate(k) && k != v && k.chars().any(char::is_alphabetic))
        .chain(maps.extra_inner.iter().map(|(k, v)| (k, v)))
        .flat_map(|(k, v)| [(k.to_uppercase(), v.to_uppercase()), (k.to_lowercase(), v.to_lowercase())])
        .filter(|(k, _)| !maps.strings.contains_key(k))
        .collect();
    maps.extra_inner.extend(variants);
    for (real, fake) in faker.digit_pairs() {
        maps.digits.insert(real.to_string(), fake.to_string());
        // ИНН числом без ведущего нуля.
        if let Some(short) = real.strip_prefix('0') {
            if short.len() >= 9 {
                maps.digits.entry(short.to_string()).or_insert_with(|| fake.to_string());
            }
        }
    }
    // Телефоны — только полные номера (10 цифр): короткие внутри текста легко совпадут случайно.
    for (real, fake) in faker.phone_pairs().filter(|(r, f)| r.len() == 10 && f.len() == 10) {
        maps.phones.insert(real.to_string(), fake.to_string());
    }
    if inner {
        maps.build_inner();
    }
    (maps, scan_extra)
}

/// Перезапись одной книги по готовым заменам.
#[allow(clippy::too_many_arguments)]
fn write_book(
    zip: &mut Zip,
    wb: &Workbook,
    rules: &[Rule],
    faker: &Faker,
    maps: &GlobalMaps,
    settings: &Settings,
    output: &Path,
    log: &mut dyn FnMut(String),
) -> Result<Report> {
    let mut report = Report { output: output.to_path_buf(), ..Default::default() };
    let (sheet_rules, _) = rules_by_sheet(wb, rules);
    let tainted = derived_formula_cells(zip, wb, rules)?;
    let no_taint = HashMap::new();
    // 3. Перезапись.
    log("Запись файла…".into());
        let sheet_by_path: HashMap<&str, usize> = wb.sheets.iter().enumerate().map(|(i, s)| (s.path.as_str(), i)).collect();
    let pivots = pivot_caches(zip)?;
    let mut pivot_map: HashMap<&str, (bool, &PivotCache)> = HashMap::new();
    for pc in &pivots {
        pivot_map.insert(pc.def_path.as_str(), (false, pc));
        if let Some(r) = &pc.records_path {
            pivot_map.insert(r.as_str(), (true, pc));
        }
    }
    let mut stats = Stats { rule_cells: vec![0; rules.len()], ..Default::default() };
    let tmp = output.with_extension("tmp");
    {
        let out_file = File::create(&tmp).with_context(|| format!("не создаётся {}", tmp.display()))?;
        let mut zw = ZipWriter::new(BufWriter::new(out_file));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let empty: Vec<(usize, &Rule)> = Vec::new();
        for i in 0..zip.len() {
            let name = zip.by_index_raw(i)?.name().to_string();
            let is_xml = name.ends_with(".xml") || name.ends_with(".rels");
            let sheet_rels = name.starts_with("xl/worksheets/_rels/") && name.ends_with(".rels");
            let other_rels = name.contains("/_rels/") || name.starts_with("_rels/");
            let new: Option<Vec<u8>> = if !is_xml || (other_rels && !sheet_rels) {
                None
            } else {
                let mut data = Vec::new();
                zip.by_index(i)?.read_to_end(&mut data)?;
                let res = if sheet_rels {
                    rewrite_hyperlink_rels(&data, maps, &mut stats)?
                } else if let Some(&si) = sheet_by_path.get(name.as_str()) {
                    let srules = sheet_rules.get(&si).unwrap_or(&empty);
                    rewrite_sheet(&data, &wb.sst, srules, tainted.get(&si).unwrap_or(&no_taint), faker, maps, &mut stats)
                        .with_context(|| format!("лист «{}»", wb.sheets[si].name))
                        .map_err(|e| e)?
                } else if Some(name.as_str()) == wb.sst_path.as_deref() {
                    rewrite_sst(&data, maps, &mut stats)?
                } else if name == wb.workbook_path {
                    rewrite_workbook(&data, settings.recalc_on_open, settings.clean_metadata, maps)?
                } else {
                    let mut pivot_out: Option<Vec<u8>> = None;
                    if let Some((is_rec, pc)) = pivot_map.get(name.as_str()) {
                        pivot_out = rewrite_pivot(&data, *is_rec, pc, rules, faker, wb.date1904)
                            .with_context(|| format!("сводная {name}"))?;
                        if pc.source.is_none() && !*is_rec {
                            report.warnings.push(format!(
                                "{name}: источник сводной не распознан — её кэш очищен, Excel пересоберёт сводную при открытии"
                            ));
                        }
                    }
                    let data: &[u8] = pivot_out.as_deref().unwrap_or(&data);
                    let overrides: &[(&[u8], &str)] = if !settings.clean_metadata {
                        &[]
                    } else if name == "docProps/core.xml" {
                        &[(b"creator", "Автор"), (b"lastModifiedBy", "Автор")]
                    } else if name == "docProps/app.xml" {
                        &[(b"Company", ""), (b"Manager", "")]
                    } else if name.starts_with("xl/comments") {
                        &[(b"author", "Автор {n}")]
                    } else {
                        &[]
                    };
                    let skip = name.ends_with("styles.xml") || name.contains("/theme/") || name.ends_with("calcChain.xml") || name == "[Content_Types].xml";
                    if skip && overrides.is_empty() {
                        pivot_out
                    } else {
                        let persons = settings.clean_metadata && name.starts_with("xl/persons/");
                        match rewrite_generic(data, maps, overrides, persons, settings.clean_metadata, &mut stats) {
                            Ok(Some(r)) => Some(r),
                            Ok(None) => pivot_out,
                            Err(e) => {
                                report.warnings.push(format!("{name}: не удалось обработать ({e}), скопирован без изменений"));
                                pivot_out
                            }
                        }
                    }
                };
                res
            };
            // Пути профиля пользователя в служебных частях: связи (гиперссылки, внешние книги), книга,
            // подключения, свойства файла (база гиперссылок). Тексты ячеек, примечаний и кэшей — данные,
            // их меняют только правила.
            let service = name.ends_with(".rels")
                || name == wb.workbook_path
                || name == "xl/connections.xml"
                || name.starts_with("docProps/");
            let new = if settings.clean_metadata && is_xml && service {
                let cur = match &new {
                    Some(b) => b.clone(),
                    None => {
                        let mut d = Vec::new();
                        zip.by_index(i)?.read_to_end(&mut d)?;
                        d
                    }
                };
                String::from_utf8(cur).ok().and_then(|t| mask_user_paths(&t)).map(String::into_bytes).or(new)
            } else {
                new
            };
            match new {
                Some(bytes) => {
                    report.parts_changed += 1;
                    zw.start_file(name.clone(), opts)?;
                    zw.write_all(&bytes)?;
                }
                None => zw.raw_copy_file(zip.by_index_raw(i)?)?,
            }
        }
        zw.finish()?.flush()?;
    }
    std::fs::rename(&tmp, &output)
        .or_else(|_| std::fs::copy(&tmp, &output).map(|_| ()).and_then(|_| std::fs::remove_file(&tmp)))
        .with_context(|| format!("не записывается {}", output.display()))?;
    report.rule_cells = stats.rule_cells;
    report.sst_replaced = stats.sst;
    report.formula_cache_replaced = stats.formula;
    report.formula_text_replaced = stats.formula_text;
    report.formula_cache_cleared = stats.formula_cleared;
    report.other_replaced = stats.other;

    Ok(report)
}

/// День, от которого Excel считает даты: 30.12.1899, в книгах «1904» — 01.01.1904.
fn date_base(d1904: bool) -> i64 {
    if d1904 {
        crate::fake::days_from_civil(1904, 1, 1)
    } else {
        crate::fake::days_from_civil(1899, 12, 30)
    }
}

/// Дата Excel (число дней) → «гггг-мм-ддT00:00:00», как в кэше сводных.
fn serial_to_iso(s: &str, d1904: bool) -> Option<String> {
    let v: f64 = s.trim().parse().ok()?;
    let min = if d1904 { 0.0 } else { 61.0 };
    if !(min..2_958_466.0).contains(&v) {
        return None;
    }
    let total = (v * 86400.0).round() as i64;
    let (y, m, d) = crate::fake::civil_from_days(date_base(d1904) + total.div_euclid(86400));
    let secs = total.rem_euclid(86400);
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60))
}

/// Правила по листам: индекс листа → (номер правила, правило). Второе — предупреждения о пропавших листах.
fn rules_by_sheet<'a>(wb: &Workbook, rules: &'a [Rule]) -> (HashMap<usize, Vec<(usize, &'a Rule)>>, Vec<String>) {
    let mut map: HashMap<usize, Vec<(usize, &Rule)>> = HashMap::new();
    let mut warnings = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        match wb.sheet_index(&r.sheet) {
            Some(s) => map.entry(s).or_default().push((i, r)),
            None => warnings.push(format!("Лист «{}» не найден — правило {} пропущено", r.sheet, r.range_text())),
        }
    }
    (map, warnings)
}

/// Что будет заменено в одном файле: значения под правилами в порядке появления.
/// Общий шаг обработки и предпросмотра — поэтому предпросмотр показывает ровно то, что попадёт в файл.
pub struct FilePlan {
    /// (номер правила, исходное значение, число ли) — уникальные по типу замены.
    pub items: Vec<(usize, String, bool)>,
    /// Сколько ячеек попадает под каждое правило (без формул и заголовков).
    pub rule_cells: Vec<usize>,
    /// Первые разные значения каждого правила — для предпросмотра. Отдельно от `items`:
    /// там значение достаётся первому правилу, а у второго правила с теми же людьми примеров бы не было.
    pub rule_samples: Vec<Vec<String>>,
    pub warnings: Vec<String>,
}

fn collect(zip: &mut Zip, wb: &Workbook, rules: &[Rule]) -> Result<FilePlan> {
    let (sheet_rules, warnings) = rules_by_sheet(wb, rules);
    let mut items: Vec<(usize, String, bool)> = Vec::new();
    let mut rule_cells = vec![0usize; rules.len()];
    let mut rule_samples: Vec<Vec<String>> = vec![Vec::new(); rules.len()];
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (si, info) in wb.sheets.iter().enumerate() {
        let Some(srules) = sheet_rules.get(&si) else { continue };
        let xml = read_part(zip, &info.path)?.with_context(|| format!("нет части {}", info.path))?;
        scan_cells(&xml, &wb.sst, |row, col, v| {
            // Кэш формулы под правилом (ФИО из другой книги, ВПР) — те же персональные данные.
            // Ошибки, логические значения, заголовки и «Итого» — нет.
            if v.special || crate::detect::is_label(&v.text) {
                return true;
            }
            if let Some((ri, r)) = srules.iter().find(|(_, r)| r.covers(row, col)) {
                rule_cells[*ri] += 1;
                let samples = &mut rule_samples[*ri];
                if samples.len() < 3 && !samples.contains(&v.text) {
                    samples.push(v.text.clone());
                }
                if seen.insert((r.kind_key(), v.text.clone())) {
                    items.push((*ri, v.text, v.numeric));
                }
            }
            true
        })?;
    }
    Ok(FilePlan { items, rule_cells, rule_samples, warnings })
}

/// Значения всех файлов и один генератор на всех: строки всех книг заняты для фейков,
/// полные ФИО из любого файла связываются с «Иванов И.И.» в другом. Третье — число записей ключа.
pub fn plan_batch(
    books: &mut [(Zip, Workbook)],
    files: &[&Path],
    rules: &[&[Rule]],
    settings: &Settings,
    key_in: Option<&Path>,
) -> Result<(Vec<FilePlan>, Faker, usize)> {
    let mut plans = Vec::new();
    for ((zip, wb), r) in books.iter_mut().zip(rules) {
        plans.push(collect(zip, wb, r)?);
    }
    let mut faker = Faker::new(settings);
    for (_, wb) in books.iter() {
        faker.set_taken(wb.sst.iter().map(String::as_str));
    }
    let mut key_entries = 0;
    if let Some(k) = key_in {
        let entries = keyfile::read(k)?;
        key_entries = entries.len();
        faker.load_entries(entries);
    }
    let all = || plans.iter().zip(rules).flat_map(|(p, r)| p.items.iter().map(move |it| (&r[it.0], it)));
    faker.prepare_people(all().filter(|(r, _)| r.kind == Kind::FullName).map(|(_, it)| it.1.as_str()));
    let todo: Vec<(&Rule, &String, bool)> = all().map(|(r, it)| (r, &it.1, it.2)).collect();
    for (r, original, numeric) in todo {
        faker.fake(r, original, numeric);
    }
    kind_conflicts(&mut plans, rules, &faker, files);
    Ok((plans, faker, key_entries))
}

/// Одно значение под правилами разных типов (ИНН в одном файле, «Текст» в другом) получает разные фейки —
/// связка между колонками и файлами рвётся. Об этом — предупреждение в каждом затронутом файле;
/// самих значений в нём нет (журнал могут переслать), только где и какие типы.
fn kind_conflicts(plans: &mut [FilePlan], rules: &[&[Rule]], faker: &Faker, files: &[&Path]) {
    let mut by_real: HashMap<&str, Vec<String>> = HashMap::new();
    for e in &faker.entries {
        by_real.entry(e.real.as_str()).or_default().push(e.kind_key.clone());
    }
    let mut found: std::collections::BTreeMap<Vec<String>, (usize, HashSet<usize>)> = Default::default();
    for p in plans.iter() {
        for (_, original, _) in &p.items {
            if original.chars().count() < 3 {
                continue;
            }
            let Some(kinds) = by_real.get(original.as_str()) else { continue };
            let fakes: HashSet<&str> = kinds.iter().filter_map(|k| faker.get(k, original)).collect();
            if fakes.len() < 2 {
                continue;
            }
            // Где это значение размечено и как: в файлах прогона и в ключе.
            let mut places: Vec<String> = Vec::new();
            let mut involved = HashSet::new();
            for (fj, q) in plans.iter().enumerate() {
                for (rj, o, _) in &q.items {
                    if o == original {
                        let r = &rules[fj][*rj];
                        places.push(format!("{}!{}!{} — {}", file_name(files[fj]), r.sheet, crate::xlsx::col_name(r.col), r.kind.title()));
                        involved.insert(fj);
                    }
                }
            }
            let here: HashSet<String> = plans.iter().zip(rules).flat_map(|(q, rs)| q.items.iter().filter(|it| &it.1 == original).map(|it| rs[it.0].kind_key())).collect();
            for k in kinds.iter().filter(|k| !here.contains(*k)) {
                if let Some((kind, _, _)) = parse_kind_key(k) {
                    places.push(format!("ключ прошлых запусков — {}", kind.title()));
                }
            }
            places.sort();
            places.dedup();
            let entry = found.entry(places).or_insert_with(|| (0, HashSet::new()));
            entry.0 += 1;
            entry.1.extend(involved);
        }
    }
    for (places, (count, involved)) in found {
        let w = format!(
            "одни и те же значения ({count}) размечены разными типами: {} — у них разные фейки, связь между колонками и файлами потеряется. Поставьте один тип",
            places.join("; ")
        );
        for fi in involved {
            if !plans[fi].warnings.contains(&w) {
                plans[fi].warnings.push(w.clone());
            }
        }
    }
}

/// Предпросмотр одного правила: число ячеек и первые пары «реальное → фейк».
#[derive(Clone, Debug, Default)]
pub struct RulePreview {
    pub cells: usize,
    pub samples: Vec<(String, String)>,
}

pub fn preview(input: &Path, profile: &Profile, key_in: Option<&Path>) -> Result<Vec<RulePreview>> {
    Ok(preview_batch(&[(input.to_path_buf(), profile.rules.clone())], &profile.settings, key_in)?.remove(0))
}

/// Предпросмотр пакета: по каждому файлу — по каждому правилу. Фейки те же, что даст `run_batch`.
pub fn preview_batch(files: &[(PathBuf, Vec<Rule>)], settings: &Settings, key_in: Option<&Path>) -> Result<Vec<Vec<RulePreview>>> {
    let mut books = Vec::new();
    for (input, _) in files {
        let mut zip = open_zip(input)?;
        let wb = Workbook::from_zip(&mut zip)?;
        books.push((zip, wb));
    }
    let rules: Vec<&[Rule]> = files.iter().map(|(_, r)| r.as_slice()).collect();
    let paths: Vec<&Path> = files.iter().map(|(p, _)| p.as_path()).collect();
    let (plans, faker, _) = plan_batch(&mut books, &paths, &rules, settings, key_in)?;
    Ok(plans
        .iter()
        .zip(&rules)
        .map(|(p, rules)| {
            p.rule_cells
                .iter()
                .zip(&p.rule_samples)
                .zip(rules.iter())
                .map(|((&cells, values), rule)| RulePreview {
                    cells,
                    samples: values
                        .iter()
                        .filter_map(|v| faker.get(&rule.kind_key(), v).map(|f| (v.clone(), f.to_string())))
                        .collect(),
                })
                .collect()
        })
        .collect())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

// ---------------------------------------------------------------- переписывание XML

fn name_of(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.name().as_ref()).into_owned()
}

fn prefix_of(e: &BytesStart) -> String {
    let n = name_of(e);
    match n.split_once(':') {
        Some((p, _)) => format!("{p}:"),
        None => String::new(),
    }
}

/// Копия элемента без атрибута `t` (тип ячейки) — тип выставляется заново.
fn copy_without(e: &BytesStart, skip: &[u8]) -> BytesStart<'static> {
    let mut n = BytesStart::new(name_of(e));
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.as_ref() != skip {
            n.push_attribute(a);
        }
    }
    n.into_owned()
}

fn is_plain_number(s: &str) -> bool {
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    !s.is_empty()
        && digits <= 15
        && s.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-')
        && s.parse::<f64>().is_ok()
        && !(s.starts_with('0') && s.len() > 1 && !s.starts_with("0."))
        && !(s.starts_with("-0") && s.len() > 2 && !s.starts_with("-0."))
}

fn write_text_elem(w: &mut Writer<Vec<u8>>, name: &str, text: &str) -> Result<()> {
    let mut t = BytesStart::new(name.to_string());
    if name.ends_with('t') {
        t.push_attribute(("xml:space", "preserve"));
    }
    w.write_event(Event::Start(t))?;
    w.write_event(Event::Text(BytesText::new(text)))?;
    w.write_event(Event::End(BytesEnd::new(name.to_string())))?;
    Ok(())
}

/// Ячейка с новым значением: число, строка (inlineStr) или пустая.
fn write_value_cell(w: &mut Writer<Vec<u8>>, start: &BytesStart, out: &str, numeric: bool) -> Result<()> {
    let p = prefix_of(start);
    let mut c = copy_without(start, b"t");
    let name = name_of(start);
    if out.is_empty() {
        w.write_event(Event::Empty(c))?;
    } else if numeric && is_plain_number(out) {
        w.write_event(Event::Start(c))?;
        write_text_elem(w, &format!("{p}v"), out)?;
        w.write_event(Event::End(BytesEnd::new(name)))?;
    } else {
        c.push_attribute(("t", "inlineStr"));
        w.write_event(Event::Start(c))?;
        w.write_event(Event::Start(BytesStart::new(format!("{p}is"))))?;
        write_text_elem(w, &format!("{p}t"), out)?;
        w.write_event(Event::End(BytesEnd::new(format!("{p}is"))))?;
        w.write_event(Event::End(BytesEnd::new(name)))?;
    }
    Ok(())
}

fn rewrite_sheet(
    xml: &[u8],
    sst: &[String],
    rules: &[(usize, &Rule)],
    tainted: &HashMap<(u32, u32), u8>,
    faker: &Faker,
    maps: &GlobalMaps,
    stats: &mut Stats,
) -> Result<Option<Vec<u8>>> {
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 4096));
    let mut buf = Vec::new();
    let mut changed = false;
    let mut row: u32 = 0;
    let mut col: i64 = -1;
    let mut cell: Option<(BytesStart<'static>, u32, u32)> = None;
    let mut inner: Vec<Event<'static>> = Vec::new();
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        if let Some((start, r, c)) = &cell {
            if let Event::End(e) = &ev {
                if e.local_name().as_ref() == b"c" {
                    let ch = process_cell(&mut w, start, *r, *c, &inner, sst, rules, tainted, faker, maps, stats)?;
                    changed |= ch;
                    cell = None;
                    inner.clear();
                    continue;
                }
            }
            inner.push(ev);
            continue;
        }
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"row" => {
                row = attr(e, b"r").and_then(|s| s.parse().ok()).unwrap_or(row + 1);
                col = -1;
            }
            Event::Start(e) if e.local_name().as_ref() == b"c" => {
                let (cc, rr) = attr(e, b"r").and_then(|s| crate::xlsx::parse_ref(&s)).unwrap_or(((col + 1) as u32, row));
                col = cc as i64;
                cell = Some((e.clone().into_owned(), rr, cc));
                continue;
            }
            Event::Empty(e) if e.local_name().as_ref() == b"c" => {
                let (cc, _) = attr(e, b"r").and_then(|s| crate::xlsx::parse_ref(&s)).unwrap_or(((col + 1) as u32, row));
                col = cc as i64;
            }
            // Вне ячеек: колонтитулы, формулы условного форматирования и проверки данных (в т.ч. в extLst).
            Event::Text(e) => {
                let t = e.unescape()?;
                if !t.trim().is_empty() {
                    if let Some(n) = replace_formula_literals(&t, maps).or_else(|| maps.replace_in(&t)) {
                        w.write_event(Event::Text(BytesText::new(&n)))?;
                        changed = true;
                        stats.other += 1;
                        continue;
                    }
                }
            }
            // Подсказки гиперссылок и тексты проверки данных.
            Event::Start(e) | Event::Empty(e) if matches!(e.local_name().as_ref(), b"hyperlink" | b"dataValidation") => {
                const TEXTS: &[&[u8]] = &[b"tooltip", b"display", b"prompt", b"error", b"promptTitle", b"errorTitle"];
                let mut n = e.clone().into_owned();
                let mut hit = false;
                for a in e.attributes().with_checks(false).flatten() {
                    if TEXTS.contains(&a.key.as_ref()) {
                        if let Some(out) = a.unescape_value().ok().and_then(|v| maps.replace_in(&v)) {
                            n = with_attr(&n, a.key.as_ref(), Some(&out));
                            hit = true;
                        }
                    }
                }
                if hit {
                    w.write_event(if matches!(ev, Event::Start(_)) { Event::Start(n) } else { Event::Empty(n) })?;
                    changed = true;
                    stats.other += 1;
                    continue;
                }
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

#[allow(clippy::too_many_arguments)]
fn process_cell(
    w: &mut Writer<Vec<u8>>,
    start: &BytesStart<'static>,
    row: u32,
    col: u32,
    inner: &[Event<'static>],
    sst: &[String],
    rules: &[(usize, &Rule)],
    tainted: &HashMap<(u32, u32), u8>,
    faker: &Faker,
    maps: &GlobalMaps,
    stats: &mut Stats,
) -> Result<bool> {
    let t = attr(start, b"t").unwrap_or_default();
    let (mut has_f, mut v, mut is_text) = (false, String::new(), String::new());
    let (mut in_v, mut in_t, mut rph) = (false, false, 0u32);
    for ev in inner {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"f" => has_f = true,
                b"v" if matches!(ev, Event::Start(_)) => in_v = true,
                b"rPh" if matches!(ev, Event::Start(_)) => rph += 1,
                b"t" if matches!(ev, Event::Start(_)) && rph == 0 => in_t = true,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"v" => in_v = false,
                b"t" => in_t = false,
                b"rPh" => rph = rph.saturating_sub(1),
                _ => {}
            },
            Event::Text(e) => {
                if in_v {
                    v.push_str(&e.unescape()?);
                } else if in_t {
                    is_text.push_str(&e.unescape()?);
                }
            }
            _ => {}
        }
    }

    // Ячейка в диапазоне правила. У формулы (ФИО из другой книги, ВПР) меняется кэш, формула остаётся.
    if t != "b" && t != "e" {
        if let Some((ri, rule)) = rules.iter().find(|(_, r)| r.covers(row, col)) {
            if let Some((original, numeric)) = resolve_value(&t, &v, &is_text, sst) {
                if let Some(out) = faker.get(&rule.kind_key(), &original) {
                    if has_f {
                        write_formula_cell(w, start, inner, Some(Some(out.to_string())), !numeric, maps, stats)?;
                    } else {
                        write_value_cell(w, start, out, numeric)?;
                    }
                    stats.rule_cells[*ri] += 1;
                    return Ok(true);
                }
            }
        }
    }

    // Строка внутри ячейки вне правил, содержащая реальное значение.
    if !has_f && t == "inlineStr" {
        if let Some(out) = maps.replace_in(&is_text) {
            write_value_cell(w, start, &out, false)?;
            stats.other += 1;
            return Ok(true);
        }
    }

    // Длинное число вне правил (ИНН, телефон числом) — то же, что под правилом здесь или в другом файле.
    if !has_f && (t.is_empty() || t == "n") && v.len() >= 9 {
        let out = maps
            .numbers
            .get(&v)
            .or_else(|| maps.strings.get(&v))
            .cloned()
            .or_else(|| v.bytes().all(|c| c.is_ascii_digit()).then(|| maps.lookup_digits(&v)).flatten());
        if let Some(out) = out {
            write_value_cell(w, start, &out, is_plain_number(&out))?;
            stats.other += 1;
            return Ok(true);
        }
    }

    let level = tainted.get(&(row, col)).copied().unwrap_or(0);

    // Значение, разлитое формулой-массивом (UNIQUE, ФИЛЬТР): <v> без <f>.
    if !has_f && t == "str" {
        match maps.replace_in(&v) {
            Some(out) => {
                let mut c = copy_without(start, b"t");
                c.push_attribute(("t", "str"));
                w.write_event(Event::Start(c))?;
                write_text_elem(w, &format!("{}v", prefix_of(start)), &out)?;
                w.write_event(Event::End(BytesEnd::new(name_of(start))))?;
                stats.formula += 1;
                return Ok(true);
            }
            None if level > 0 => {
                w.write_event(Event::Empty(copy_without(start, b"t")))?;
                stats.formula_cleared += 1;
                return Ok(true);
            }
            None => {}
        }
    }

    if has_f {
        // Кэш результата формулы вне правил: известное значение — заменить; производное от ячеек
        // с ПДн (ЛЕВСИМВ(ФИО), ТЕКСТ(дата), =C2) — убрать, Excel пересчитает от фейков при открытии.
        // Числа очищаются, если они производные от «сильных» данных (дата, телефон, номер документа)
        // или совпадают с исходным значением; счётчики и суммы по именам остаются.
        let is_str = t == "str";
        let derived = level > 0 && !v.is_empty();
        let new_v = if is_str {
            match maps.replace_in(&v) {
                Some(out) => Some(Some(out)),
                None if derived => Some(None),
                None => None,
            }
        } else if derived && (t.is_empty() || t == "n") && (level >= 2 || maps.is_value_original(&v)) {
            Some(maps.numbers.get(&v).filter(|o| is_plain_number(o)).cloned())
        } else if (t.is_empty() || t == "n") && maps.is_known_number(&v) {
            // ВПР из справочника вернул ИНН или телефон числом — тот же фейк, что и в правиле.
            Some(maps.numbers.get(&v).or_else(|| maps.strings.get(&v)).filter(|o| is_plain_number(o)).cloned())
        } else {
            None
        };
        return write_formula_cell(w, start, inner, new_v, is_str, maps, stats);
    }

    w.write_event(Event::Start(start.clone()))?;
    for ev in inner {
        w.write_event(ev.clone())?;
    }
    w.write_event(Event::End(BytesEnd::new(name_of(start))))?;
    Ok(false)
}

/// Ячейка с формулой: строковые литералы в тексте формулы заменяются всегда; кэш результата —
/// `None` оставить, `Some(Some(x))` заменить на x, `Some(None)` убрать (вместе с типом).
fn write_formula_cell(
    w: &mut Writer<Vec<u8>>,
    start: &BytesStart<'static>,
    inner: &[Event<'static>],
    new_v: Option<Option<String>>,
    keep_str: bool,
    maps: &GlobalMaps,
    stats: &mut Stats,
) -> Result<bool> {
    let p = prefix_of(start);
    let mut changed = false;
    let head = match &new_v {
        None => start.clone(),
        Some(v) => {
            let mut c = copy_without(start, b"t");
            if v.as_deref().is_some_and(|x| keep_str || !is_plain_number(x)) {
                c.push_attribute(("t", "str"));
            }
            c
        }
    };
    w.write_event(Event::Start(head))?;
    let (mut in_f, mut in_v) = (false, false);
    for ev in inner {
        let ln = match ev {
            Event::Start(e) | Event::Empty(e) => e.local_name().as_ref().to_vec(),
            Event::End(e) => e.local_name().as_ref().to_vec(),
            _ => Vec::new(),
        };
        if new_v.is_some() && ln == b"v" {
            in_v = matches!(ev, Event::Start(_));
            continue;
        }
        if in_v {
            continue;
        }
        match ev {
            Event::Start(_) if ln == b"f" => in_f = true,
            Event::Text(e) if in_f => {
                if let Some(nf) = replace_formula_literals(&e.unescape()?, maps) {
                    w.write_event(Event::Text(BytesText::new(&nf)))?;
                    stats.formula_text += 1;
                    changed = true;
                    continue;
                }
            }
            _ => {}
        }
        w.write_event(ev.clone())?;
        let f_closed = (matches!(ev, Event::End(_)) || matches!(ev, Event::Empty(_))) && ln == b"f";
        if f_closed {
            in_f = false;
            if let Some(Some(out)) = &new_v {
                write_text_elem(w, &format!("{p}v"), out)?;
            }
        }
    }
    w.write_event(Event::End(BytesEnd::new(name_of(start))))?;
    match &new_v {
        Some(Some(_)) => {
            stats.formula += 1;
            changed = true;
        }
        Some(None) => {
            stats.formula_cleared += 1;
            changed = true;
        }
        None => {}
    }
    Ok(changed)
}

/// Заменяет строковые литералы "…" в тексте формулы, если в них реальные значения.
fn replace_formula_literals(f: &str, maps: &GlobalMaps) -> Option<String> {
    let chars: Vec<char> = f.chars().collect();
    let mut out = String::with_capacity(f.len());
    let mut changed = false;
    let mut i = 0;
    while i < chars.len() {
        // Структурная ссылка «План[Иванов Иван]» / «[@Иванов Иван]»: имя колонки — текст шапки.
        if chars[i] == '[' {
            if let Some(len) = chars[i + 1..].iter().position(|&c| c == ']' || c == '[') {
                let j = i + 1 + len;
                if chars[j] == ']' {
                    let inner: String = chars[i + 1..j].iter().collect();
                    let (at, name) = match inner.strip_prefix('@') {
                        Some(n) => ("@", n.to_string()),
                        None => ("", inner.clone()),
                    };
                    if let Some(rep) = maps.cell_text(&name) {
                        let esc: String = rep.chars().flat_map(|c| if "[]#'".contains(c) { vec!['\'', c] } else { vec![c] }).collect();
                        out.push_str(&format!("[{at}{esc}]"));
                        changed = true;
                    } else {
                        out.push_str(&format!("[{inner}]"));
                    }
                    i = j + 1;
                    continue;
                }
            }
        }
        if chars[i] != '"' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut lit = String::new();
        while j < chars.len() {
            if chars[j] == '"' {
                if chars.get(j + 1) == Some(&'"') {
                    lit.push('"');
                    j += 2;
                    continue;
                }
                break;
            }
            lit.push(chars[j]);
            j += 1;
        }
        match maps.replace_in(&lit) {
            Some(rep) if j < chars.len() => {
                out.push('"');
                out.push_str(&rep.replace('"', "\"\""));
                out.push('"');
                changed = true;
            }
            _ => out.extend(&chars[i..=j.min(chars.len() - 1)]),
        }
        i = j + 1;
    }
    changed.then_some(out)
}

/// Общие строки: запись целиком заменяется, если её текст — реальное значение из правил.
fn rewrite_sst(xml: &[u8], maps: &GlobalMaps, stats: &mut Stats) -> Result<Option<Vec<u8>>> {
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 4096));
    let mut buf = Vec::new();
    let mut changed = false;
    let mut si: Option<BytesStart<'static>> = None;
    let mut inner: Vec<Event<'static>> = Vec::new();
    let mut text = String::new();
    let (mut in_t, mut rph) = (false, 0u32);
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        if let Some(start) = &si {
            match &ev {
                Event::End(e) if e.local_name().as_ref() == b"si" => {
                    if let Some(out) = maps.sst(&text).cloned().or_else(|| maps.replace_in(&text)) {
                        let p = prefix_of(start);
                        w.write_event(Event::Start(start.clone()))?;
                        write_text_elem(&mut w, &format!("{p}t"), &out)?;
                        stats.sst += 1;
                        changed = true;
                    } else {
                        w.write_event(Event::Start(start.clone()))?;
                        for e in inner.drain(..) {
                            w.write_event(e)?;
                        }
                    }
                    w.write_event(ev)?;
                    si = None;
                    inner.clear();
                    continue;
                }
                Event::Start(e) => match e.local_name().as_ref() {
                    b"rPh" => rph += 1,
                    b"t" if rph == 0 => in_t = true,
                    _ => {}
                },
                Event::End(e) => match e.local_name().as_ref() {
                    b"rPh" => rph = rph.saturating_sub(1),
                    b"t" => in_t = false,
                    _ => {}
                },
                Event::Text(e) if in_t => text.push_str(&e.unescape()?),
                Event::CData(e) if in_t => text.push_str(&String::from_utf8_lossy(e)),
                _ => {}
            }
            inner.push(ev);
            continue;
        }
        match &ev {
            Event::Eof => break,
            Event::Start(e) if e.local_name().as_ref() == b"si" => {
                si = Some(e.clone().into_owned());
                text.clear();
                in_t = false;
                rph = 0;
                continue;
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

/// Просит Excel пересчитать формулы при открытии (`calcPr fullCalcOnLoad="1"`) и заменяет
/// реальные значения в определённых именах (`Ответственный = "Иванов Иван"`).
/// Имя пользователя в пути профиля заменяется на «user»: `C:\Users\<имя>\…`, `file:///C:/Users/<имя>/…`,
/// `D:\Documents and Settings\<имя>\…`, `/Users/<имя>/…` (macOS). Остальной путь не меняется.
/// None — путей профиля нет.
fn mask_user_paths(s: &str) -> Option<String> {
    const HOMES: &[&str] = &["users", "documents and settings"];
    let low = s.to_ascii_lowercase();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let sep = b[i];
        let home = (sep == b'\\' || sep == b'/')
            .then(|| HOMES.iter().find(|h| low[i + 1..].starts_with(*h) && b.get(i + 1 + h.len()) == Some(&sep)))
            .flatten();
        let Some(home) = home else {
            i += 1;
            continue;
        };
        // Путь от корня диска (C:\Users) или, для macOS, от начала значения (/Users).
        let before = &low[..i];
        let rooted = (i >= 2 && b[i - 1] == b':' && b[i - 2].is_ascii_alphabetic())
            || (sep == b'/' && (before.is_empty() || before.ends_with(['"', '\'', '>']) || before.ends_with("file://")));
        let start = i + 2 + home.len();
        let end = s[start..].find(['\\', '/', '"', '\'', '<']).map_or(s.len(), |e| start + e);
        if rooted && end > start && &s[start..end] != "user" {
            out.push_str(&s[last..start]);
            out.push_str("user");
            last = end;
        }
        i = start;
    }
    (last > 0).then(|| {
        out.push_str(&s[last..]);
        out
    })
}

fn rewrite_workbook(xml: &[u8], recalc: bool, clean_meta: bool, maps: &GlobalMaps) -> Result<Option<Vec<u8>>> {
    // Элементы, перед которыми по схеме должен стоять calcPr, если его не было.
    const AFTER_CALC: &[&[u8]] = &[
        b"oleSize", b"customWorkbookViews", b"pivotCaches", b"smartTagPr", b"smartTagTypes",
        b"webPublishing", b"fileRecoveryPr", b"webPublishObjects", b"extLst",
    ];
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 64));
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut done = !recalc;
    let mut changed = recalc;
    let mut in_name = false;
    let mut prefix = String::new();
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        let calc_pr = |prefix: &str| {
            let mut c = BytesStart::new(format!("{prefix}calcPr"));
            c.push_attribute(("fullCalcOnLoad", "1"));
            Event::Empty(c)
        };
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let ln = e.local_name().as_ref().to_vec();
                if depth == 0 {
                    prefix = prefix_of(e);
                }
                if ln == b"definedName" && matches!(ev, Event::Start(_)) {
                    in_name = true;
                }
                // Имя пользователя (защита от записи) и папка, где Excel сохранял файл: в ней бывает
                // имя пользователя или заказчика. Элементы остаются — меняется только значение.
                let meta: Option<(&[u8], &str)> = match ln.as_slice() {
                    b"fileSharing" if clean_meta => Some((b"userName", "Автор")),
                    b"absPath" if clean_meta => Some((b"url", r"C:\")),
                    _ => None,
                };
                if let Some((key, val)) = meta.filter(|(k, _)| e.attributes().with_checks(false).flatten().any(|a| a.key.as_ref() == *k)) {
                    let mut c = BytesStart::new(name_of(e));
                    for a in e.attributes().with_checks(false).flatten() {
                        if a.key.as_ref() == key {
                            c.push_attribute((String::from_utf8_lossy(key).as_ref(), val));
                        } else {
                            c.push_attribute(a);
                        }
                    }
                    let start = matches!(ev, Event::Start(_));
                    w.write_event(if start { Event::Start(c) } else { Event::Empty(c) })?;
                    changed = true;
                    if start {
                        depth += 1;
                    }
                    continue;
                }
                if recalc && depth == 1 && ln == b"calcPr" {
                    let mut c = copy_without(e, b"fullCalcOnLoad");
                    c.push_attribute(("fullCalcOnLoad", "1"));
                    w.write_event(if matches!(ev, Event::Start(_)) { Event::Start(c) } else { Event::Empty(c) })?;
                    done = true;
                    if matches!(ev, Event::Start(_)) {
                        depth += 1;
                    }
                    continue;
                }
                if depth == 1 && !done && AFTER_CALC.contains(&ln.as_slice()) {
                    w.write_event(calc_pr(&prefix))?;
                    done = true;
                }
                if matches!(ev, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == b"definedName" {
                    in_name = false;
                }
                if depth == 1 && !done {
                    w.write_event(calc_pr(&prefix))?;
                    done = true;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(e) if in_name => {
                let t = e.unescape()?;
                if let Some(n) = replace_formula_literals(&t, maps).or_else(|| maps.replace_in(&t)) {
                    w.write_event(Event::Text(BytesText::new(&n)))?;
                    changed = true;
                    continue;
                }
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

/// Прочие части (сводные, диаграммы, примечания, рисунки, свойства файла):
/// заменяются текстовые узлы и атрибуты `v`, целиком совпадающие с реальными значениями.
fn rewrite_generic(
    xml: &[u8],
    maps: &GlobalMaps,
    overrides: &[(&[u8], &str)],
    persons: bool,
    clean_meta: bool,
    stats: &mut Stats,
) -> Result<Option<Vec<u8>>> {
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 1024));
    let mut buf = Vec::new();
    let mut changed = false;
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut person_n = 0;
    let mut override_n = 0;
    let mut events = Vec::new();
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        if matches!(ev, Event::Eof) {
            break;
        }
        events.push(ev);
    }
    changed |= merge_split_runs(&mut events, maps, stats)?;
    for ev in events {
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let is_start = matches!(ev, Event::Start(_));
                let ln = e.local_name().as_ref().to_vec();
                // Новое значение атрибута; None — удалить атрибут.
                let mut new_attrs: Vec<(Vec<u8>, Option<String>)> = Vec::new();
                // Границы поля в кэше сводной (min/max) снимаются обе сразу, если хоть одна — реальное
                // значение: одна граница без другой — повреждённый для Excel файл. Границы группировки дат
                // (rangePr startDate/endDate) не трогаем: Excel требует их согласованности с группами.
                const BOUNDS: &[&[u8]] = &[b"minValue", b"maxValue", b"minDate", b"maxDate"];
                let bound_hit = ln == b"sharedItems" && e.attributes().with_checks(false).flatten().any(|a| {
                    BOUNDS.contains(&a.key.as_ref()) && a.unescape_value().is_ok_and(|v| maps.is_known_number(&v) || maps.any(&v).is_some())
                });
                for a in e.attributes().with_checks(false).flatten() {
                    let key = a.key.as_ref().to_vec();
                    if clean_meta && (key == b"userName" || (ln == b"userInfo" && key == b"name")) {
                        // Журнал изменений общей книги: кто вносил правки.
                        new_attrs.push((key, Some("Автор".into())));
                    } else if persons && ln == b"person" && (key == b"displayName" || key == b"userId") {
                        if key == b"displayName" {
                            person_n += 1;
                        }
                        let val = if key == b"displayName" { format!("Автор {}", person_n.max(1)) } else { format!("author{}", person_n.max(1)) };
                        new_attrs.push((key, Some(val)));
                    } else if key == b"v" {
                        if let Some(out) = a.unescape_value().ok().and_then(|v| maps.any(&v).cloned()) {
                            new_attrs.push((key, Some(out)));
                        }
                    } else if ln == b"tableColumn" && key == b"name" {
                        // Имя колонки умной таблицы обязано совпадать с текстом ячейки шапки.
                        if let Some(out) = a.unescape_value().ok().and_then(|v| maps.cell_text(&v)) {
                            new_attrs.push((key, Some(out)));
                        }
                    } else if bound_hit && BOUNDS.contains(&key.as_slice()) {
                        new_attrs.push((key, None));
                    } else if clean_meta && ln == b"pivotCacheDefinition" && key == b"refreshedBy" {
                        new_attrs.push((key, Some("Автор".into())));
                    } else if ln == b"cacheField" && key == b"name" {
                        // Поле сводной называется по шапке источника — переименовываем вместе с ней.
                        if let Some(out) = a.unescape_value().ok().and_then(|v| maps.cell_text(&v)) {
                            new_attrs.push((key, Some(out)));
                        }
                    }
                }
                if !new_attrs.is_empty() {
                    let mut n = BytesStart::new(name_of(e));
                    for a in e.attributes().with_checks(false).flatten() {
                        match new_attrs.iter().find(|(k, _)| k.as_slice() == a.key.as_ref()) {
                            Some((k, Some(v))) => n.push_attribute((String::from_utf8_lossy(k).as_ref(), v.as_str())),
                            Some((_, None)) => {}
                            None => n.push_attribute(a),
                        }
                    }
                    changed = true;
                    stats.other += 1;
                    if is_start {
                        stack.push(ln);
                    }
                    w.write_event(if is_start { Event::Start(n) } else { Event::Empty(n) })?;
                    continue;
                }
                if is_start {
                    stack.push(ln);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(e) => {
                let top = stack.last().map(|v| v.as_slice()).unwrap_or(b"");
                if let Some((_, repl)) = overrides.iter().find(|(n, _)| *n == top) {
                    // «{n}» — номер: авторы примечаний должны остаться разными.
                    let repl = if repl.contains("{n}") {
                        override_n += 1;
                        repl.replace("{n}", &override_n.to_string())
                    } else {
                        repl.to_string()
                    };
                    if e.unescape().map(|t| t != repl).unwrap_or(true) {
                        w.write_event(Event::Text(BytesText::new(&repl)))?;
                        changed = true;
                        continue;
                    }
                } else {
                    let text = e.unescape()?;
                    if !text.trim().is_empty() {
                        if let Some(out) = maps.replace_in(&text) {
                            w.write_event(Event::Text(BytesText::new(&out)))?;
                            changed = true;
                            stats.other += 1;
                            continue;
                        }
                    }
                }
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

/// Текст абзаца, разрезанный на фрагменты форматирования (<r><t>Иванов Иван</t></r><r><t> Иванович</t></r>
/// в примечаниях и надписях): заменяется целиком — склеенный текст в первый фрагмент, остальные пустеют.
fn merge_split_runs(events: &mut [Event<'static>], maps: &GlobalMaps, stats: &mut Stats) -> Result<bool> {
    let local = |ev: &Event| match ev {
        Event::Start(e) | Event::Empty(e) => Some(e.local_name().as_ref().to_vec()),
        Event::End(e) => Some(e.local_name().as_ref().to_vec()),
        _ => None,
    };
    let mut changed = false;
    let mut i = 0;
    while i < events.len() {
        let is_para = matches!(events[i], Event::Start(_)) && local(&events[i]).is_some_and(|n| n == b"text" || n == b"p");
        if !is_para {
            i += 1;
            continue;
        }
        let mut depth = 0i32;
        let mut j = i;
        while j < events.len() {
            match events[j] {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let (mut idx, mut joined, mut in_t) = (Vec::new(), String::new(), false);
        for k in i + 1..j.min(events.len()) {
            let n = local(&events[k]);
            match &events[k] {
                Event::Start(_) if n.as_deref() == Some(b"t") => in_t = true,
                Event::End(_) if n.as_deref() == Some(b"t") => in_t = false,
                Event::Text(t) if in_t => {
                    joined.push_str(&t.unescape()?);
                    idx.push(k);
                }
                _ => {}
            }
        }
        if idx.len() >= 2 {
            let pieces: Vec<String> = idx
                .iter()
                .map(|&k| match &events[k] {
                    Event::Text(t) => t.unescape().map(|x| x.into_owned()).unwrap_or_default(),
                    _ => String::new(),
                })
                .collect();
            let separately: String = pieces.iter().map(|p| maps.replace_in(p).unwrap_or_else(|| p.clone())).collect();
            if let Some(new) = maps.replace_in(&joined).filter(|n| *n != separately) {
                if let Event::Start(st) = &events[idx[0] - 1] {
                    events[idx[0] - 1] = Event::Start(with_attr(st, b"xml:space", Some("preserve")));
                }
                events[idx[0]] = Event::Text(BytesText::new(&new).into_owned());
                for &k in &idx[1..] {
                    events[k] = Event::Text(BytesText::new("").into_owned());
                }
                changed = true;
                stats.other += 1;
            }
        }
        i = j + 1;
    }
    Ok(changed)
}

/// Ссылки «mailto:» в гиперссылках листа (xl/worksheets/_rels/sheetN.xml.rels).
fn rewrite_hyperlink_rels(xml: &[u8], maps: &GlobalMaps, stats: &mut Stats) -> Result<Option<Vec<u8>>> {
    let mut rd = Reader::from_reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 256));
    let mut buf = Vec::new();
    let mut changed = false;
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if let Some(target) = attr(e, b"Target") {
                    if let Some(rest) = target.strip_prefix("mailto:") {
                        let (addr, query) = rest.split_once('?').map_or((rest, ""), |(a, q)| (a, q));
                        if let Some(out) = maps.any(addr) {
                            let new = if query.is_empty() { format!("mailto:{out}") } else { format!("mailto:{out}?{query}") };
                            let n = with_attr(e, b"Target", Some(&new));
                            w.write_event(if matches!(ev, Event::Start(_)) { Event::Start(n) } else { Event::Empty(n) })?;
                            changed = true;
                            stats.other += 1;
                            continue;
                        }
                    }
                }
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(changed.then(|| w.into_inner()))
}

// ---------------------------------------------------------------- формулы, зависящие от ПДн

/// Ссылка из текста формулы: лист (None — свой) и диапазон; `abs` — признаки «$» у колонок и строк.
#[derive(Clone, Debug, PartialEq)]
struct FormulaRef {
    sheet: Option<String>,
    c1: u32,
    r1: u32,
    c2: u32,
    r2: u32,
    abs: [bool; 4],
}

/// «$A$1», «A1», «A» (только в диапазоне «A:A»): колонка, «$» у колонки, строка, «$» у строки.
fn parse_ref_token(t: &str) -> Option<(u32, bool, Option<u32>, bool)> {
    let (ac, s) = match t.strip_prefix('$') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let letters: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let rest = &s[letters.len()..];
    if rest.is_empty() {
        return Some((crate::xlsx::parse_col(&letters)?, ac, None, false));
    }
    let (ar, digits) = match rest.strip_prefix('$') {
        Some(d) => (true, d),
        None => (false, rest),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((crate::xlsx::parse_col(&letters)?, ac, Some(digits.parse().ok()?), ar))
}

/// Ссылки на ячейки в тексте формулы. Строки "…", функции, внешние книги [1] и структурные
/// ссылки пропускаются (структурные и имена — известное ограничение).
fn formula_refs(f: &str) -> Vec<FormulaRef> {
    let ch: Vec<char> = f.chars().collect();
    let n = ch.len();
    let is_tok = |c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '$';
    let mut out = Vec::new();
    let (mut i, mut external) = (0, false);
    while i < n {
        let c = ch[i];
        if c == '"' {
            i += 1;
            while i < n && !(ch[i] == '"' && ch.get(i + 1) != Some(&'"')) {
                i += if ch[i] == '"' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        if c == '[' {
            let mut depth = 0;
            while i < n {
                if ch[i] == '[' {
                    depth += 1;
                } else if ch[i] == ']' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
            external = true;
            continue;
        }
        let mut sheet = None;
        if c == '\'' {
            let mut s = String::new();
            i += 1;
            while i < n {
                if ch[i] == '\'' {
                    if ch.get(i + 1) == Some(&'\'') {
                        s.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                s.push(ch[i]);
                i += 1;
            }
            i += 1;
            if ch.get(i) != Some(&'!') {
                continue;
            }
            sheet = Some(s);
            i += 1;
        }
        let t0 = i;
        while i < n && is_tok(ch[i]) {
            i += 1;
        }
        if i == t0 {
            external = false;
            i += 1;
            continue;
        }
        let mut tok: String = ch[t0..i].iter().collect();
        if sheet.is_none() && ch.get(i) == Some(&'!') {
            sheet = Some(tok);
            i += 1;
            let t1 = i;
            while i < n && is_tok(ch[i]) {
                i += 1;
            }
            tok = ch[t1..i].iter().collect();
        }
        if ch.get(i) == Some(&'(') {
            external = false;
            continue;
        }
        let ext = std::mem::replace(&mut external, false);
        let Some(a) = parse_ref_token(&tok) else { continue };
        let mut b = None;
        if ch.get(i) == Some(&':') {
            let t2 = i + 1;
            let mut k = t2;
            while k < n && is_tok(ch[k]) {
                k += 1;
            }
            let tok2: String = ch[t2..k].iter().collect();
            if let Some(bb) = parse_ref_token(&tok2) {
                b = Some(bb);
                i = k;
            }
        }
        if ext {
            continue;
        }
        let b = match b {
            Some(b) => b,
            None if a.2.is_some() => a,
            None => continue, // «A» без двоеточия — это имя, а не колонка
        };
        let (r1, r2) = match (a.2, b.2) {
            (Some(x), Some(y)) => (x.min(y), x.max(y)),
            _ => (1, u32::MAX),
        };
        out.push(FormulaRef { sheet, c1: a.0.min(b.0), r1, c2: a.0.max(b.0), r2, abs: [a.1, a.3, b.1, b.3] });
    }
    out
}

/// Структурные ссылки формулы: (таблица или None — «своя», имена колонок).
/// «Таблица1[ФИО]», «[@ФИО]», «[@[Дата рождения]]», «Таблица1[[#This Row],[Дата рождения]]».
fn struct_refs(f: &str) -> Vec<(Option<String>, Vec<String>)> {
    let ch: Vec<char> = f.chars().collect();
    let n = ch.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if ch[i] == '"' {
            i += 1;
            while i < n && !(ch[i] == '"' && ch.get(i + 1) != Some(&'"')) {
                i += if ch[i] == '"' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        if ch[i] != '[' {
            i += 1;
            continue;
        }
        let mut k = i;
        while k > 0 && (ch[k - 1].is_alphanumeric() || ch[k - 1] == '_' || ch[k - 1] == '.' || ch[k - 1] == '\\') {
            k -= 1;
        }
        let table: String = ch[k..i].iter().collect();
        let (start, mut depth, mut j) = (i, 0, i);
        while j < n {
            match ch[j] {
                '\'' => {
                    j += 2;
                    continue;
                }
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let inner: String = ch[(start + 1).min(n)..j.min(n)].iter().collect();
        i = j + 1;
        if inner.chars().all(|c| c.is_ascii_digit()) {
            continue; // внешняя книга [1]
        }
        let mut items = Vec::new();
        if inner.contains('[') {
            let (mut d, mut cur) = (0, String::new());
            let mut it = inner.chars().peekable();
            while let Some(c) = it.next() {
                match c {
                    '\'' => {
                        cur.push(c);
                        if let Some(x) = it.next() {
                            cur.push(x);
                        }
                    }
                    '[' => {
                        d += 1;
                        cur.push(c);
                    }
                    ']' => {
                        d -= 1;
                        cur.push(c);
                    }
                    ',' if d == 0 => items.push(std::mem::take(&mut cur)),
                    _ => cur.push(c),
                }
            }
            items.push(cur);
        } else {
            items.push(inner);
        }
        let cols: Vec<String> = items
            .iter()
            .map(|x| x.trim().trim_start_matches('@').trim_start_matches('[').trim_end_matches(']').to_string())
            .filter(|x| !x.is_empty() && !x.starts_with('#'))
            .map(|x| x.replace("'[", "[").replace("']", "]").replace("'#", "#").replace("''", "'"))
            .collect();
        if !cols.is_empty() {
            out.push(((!table.is_empty()).then_some(table), cols));
        }
    }
    out
}

/// Насколько чувствительны данные правила для производных чисел: 2 — дата, телефон, номер
/// документа (ГОД(ДР) раскрывает год рождения); 1 — имена, адреса (СЧЁТЕСЛИ по ним — просто счётчик).
fn rule_level(kind: Kind) -> u8 {
    match kind {
        Kind::BirthDate | Kind::Phone | Kind::Inn | Kind::Snils | Kind::Passport | Kind::Account => 2,
        _ => 1,
    }
}

/// Ячейки формул (и разлитые ими значения), результат которых зависит от ячеек под правилами —
/// прямо или через другие формулы. Значение — уровень (см. `rule_level`).
/// Обход по зависимостям: каждая формула пересматривается не больше двух раз.
fn derived_formula_cells(zip: &mut Zip, wb: &Workbook, rules: &[Rule]) -> Result<HashMap<usize, HashMap<(u32, u32), u8>>> {
    type Range = (usize, u32, u32, u32, u32);
    struct Formula {
        sheet: usize,
        row: u32,
        col: u32,
        refs: Vec<Range>,
        spill: Option<(u32, u32, u32, u32)>,
    }
    let tables = crate::xlsx::sheet_tables(zip, wb)?;
    let mut forms: Vec<Formula> = Vec::new();
    for (si, info) in wb.sheets.iter().enumerate() {
        let Some(xml) = read_part(zip, &info.path)? else { continue };
        let mut masters: HashMap<String, (u32, u32, String)> = HashMap::new();
        for (row, col, text, shared, array_ref) in collect_formulas(&xml)? {
            let (text, dr, dc) = match &shared {
                Some(id) if text.trim().is_empty() => match masters.get(id) {
                    Some((mr, mc, t)) => (t.clone(), row as i64 - *mr as i64, col as i64 - *mc as i64),
                    None => continue,
                },
                Some(id) => {
                    masters.insert(id.clone(), (row, col, text.clone()));
                    (text, 0, 0)
                }
                None => (text, 0, 0),
            };
            let shift = |v: u32, abs: bool, d: i64| if abs || v == u32::MAX { v } else { (v as i64 + d).max(0) as u32 };
            let mut refs: Vec<Range> = formula_refs(&text)
                .into_iter()
                .filter_map(|r| {
                    let sheet = match &r.sheet {
                        None => si,
                        Some(name) => wb.sheet_index(name)?,
                    };
                    Some((sheet, shift(r.c1, r.abs[0], dc), shift(r.r1, r.abs[1], dr), shift(r.c2, r.abs[2], dc), shift(r.r2, r.abs[3], dr)))
                })
                .collect();
            for (table, cols) in struct_refs(&text) {
                let t = match &table {
                    Some(name) => tables.iter().find(|t| t.name.eq_ignore_ascii_case(name)),
                    None => tables.iter().find(|t| t.sheet == si && (t.c1..=t.c2).contains(&col) && (t.r1..=t.r2).contains(&row)),
                };
                let Some(t) = t else { continue };
                for c in cols {
                    if let Some(k) = t.columns.iter().position(|x| x.eq_ignore_ascii_case(&c)) {
                        let cc = t.c1 + k as u32;
                        refs.push((t.sheet, cc, t.data_r1, cc, t.data_r2));
                    }
                }
            }
            let spill = array_ref.and_then(|r| parse_range(&r)).and_then(|(c1, c2, r1, r2)| Some((c1, r1, c2, r2?)));
            forms.push(Formula { sheet: si, row, col, refs, spill });
        }
    }

    let ruled: Vec<(usize, &Rule)> = rules.iter().filter_map(|r| wb.sheet_index(&r.sheet).map(|s| (s, r))).collect();
    let rule_hit = |&(s, c1, r1, c2, r2): &Range| -> u8 {
        ruled
            .iter()
            .filter(|(rs, r)| *rs == s && r.col >= c1 && r.col <= c2 && r.row_from <= r2 && r.row_to.unwrap_or(u32::MAX) >= r1)
            .map(|(_, r)| rule_level(r.kind))
            .max()
            .unwrap_or(0)
    };

    // Кто на что ссылается: одиночные ячейки — по адресу, диапазоны — по колонкам (одинаковые
    // диапазоны склеены), очень широкие — отдельным списком.
    let mut by_cell: HashMap<(usize, u32, u32), Vec<usize>> = HashMap::new();
    let mut range_ids: HashMap<Range, usize> = HashMap::new();
    let mut ranges: Vec<(Range, Vec<usize>)> = Vec::new();
    let mut by_col: HashMap<(usize, u32), Vec<usize>> = HashMap::new();
    let mut wide: Vec<usize> = Vec::new();
    let mut level = vec![0u8; forms.len()];
    let mut queue: std::collections::VecDeque<usize> = Default::default();
    for (k, f) in forms.iter().enumerate() {
        for r in &f.refs {
            let l = rule_hit(r);
            if l > level[k] {
                level[k] = l;
            }
            if r.1 == r.3 && r.2 == r.4 {
                by_cell.entry((r.0, r.1, r.2)).or_default().push(k);
            } else {
                let id = *range_ids.entry(*r).or_insert_with(|| {
                    ranges.push((*r, Vec::new()));
                    let id = ranges.len() - 1;
                    if r.3 - r.1 > 64 {
                        wide.push(id);
                    } else {
                        for c in r.1..=r.3 {
                            by_col.entry((r.0, c)).or_default().push(id);
                        }
                    }
                    id
                });
                ranges[id].1.push(k);
            }
        }
        if level[k] > 0 {
            queue.push_back(k);
        }
    }
    while let Some(k) = queue.pop_front() {
        let f = &forms[k];
        let l = level[k];
        let mut cells = vec![(f.row, f.col)];
        if let Some((c1, r1, c2, r2)) = f.spill {
            if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 <= 100_000 {
                cells = (r1..=r2).flat_map(|r| (c1..=c2).map(move |c| (r, c))).collect();
            }
        }
        let mut deps: Vec<usize> = Vec::new();
        for (r, c) in cells {
            if let Some(v) = by_cell.get(&(f.sheet, c, r)) {
                deps.extend(v);
            }
            for &id in by_col.get(&(f.sheet, c)).into_iter().flatten().chain(wide.iter()) {
                let ((s, c1, r1, c2, r2), users) = &ranges[id];
                if *s == f.sheet && (*c1..=*c2).contains(&c) && (*r1..=*r2).contains(&r) {
                    deps.extend(users);
                }
            }
        }
        for d in deps {
            if level[d] < l {
                level[d] = l;
                queue.push_back(d);
            }
        }
    }

    let mut out: HashMap<usize, HashMap<(u32, u32), u8>> = HashMap::new();
    for (k, f) in forms.iter().enumerate() {
        if level[k] == 0 {
            continue;
        }
        let m = out.entry(f.sheet).or_default();
        m.insert((f.row, f.col), level[k]);
        if let Some((c1, r1, c2, r2)) = f.spill {
            if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 <= 100_000 {
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        let e = m.entry((r, c)).or_insert(0);
                        *e = (*e).max(level[k]);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Формулы листа: (строка, колонка, текст, номер общей формулы, диапазон формулы-массива).
fn collect_formulas(xml: &[u8]) -> Result<Vec<(u32, u32, String, Option<String>, Option<String>)>> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let (mut row, mut col): (u32, i64) = (0, -1);
    let mut cur: Option<(u32, u32, String, Option<String>, Option<String>)> = None;
    let mut in_f = false;
    loop {
        let ev = rd.read_event_into(&mut buf)?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"row" => {
                    row = attr(&e, b"r").and_then(|s| s.parse().ok()).unwrap_or(row + 1);
                    col = -1;
                }
                b"c" => {
                    let (cc, rr) = attr(&e, b"r").and_then(|s| crate::xlsx::parse_ref(&s)).unwrap_or(((col + 1) as u32, row));
                    col = cc as i64;
                    cur = Some((rr, cc, String::new(), None, None));
                }
                b"f" => {
                    if let Some(c) = cur.as_mut() {
                        match attr(&e, b"t").as_deref() {
                            Some("shared") => c.3 = attr(&e, b"si"),
                            Some("array") => c.4 = attr(&e, b"ref"),
                            _ => {}
                        }
                        if empty {
                            out.push(c.clone());
                        } else {
                            in_f = true;
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_f => {
                if let Some(c) = cur.as_mut() {
                    c.2.push_str(&t.unescape()?);
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"f" => {
                in_f = false;
                if let Some(c) = &cur {
                    out.push(c.clone());
                }
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// ---------------------------------------------------------------- сводные таблицы

/// «1982-02-16T00:00:00» → дата Excel «30000».
fn iso_to_serial(s: &str, d1904: bool) -> Option<String> {
    let (date, time) = s.split_once('T').unwrap_or((s, "00:00:00"));
    let p: Vec<i64> = date.split('-').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    let t: Vec<i64> = time.split(':').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if p.len() != 3 || t.len() != 3 {
        return None;
    }
    let days = crate::fake::days_from_civil(p[0], p[1], p[2]) - date_base(d1904);
    let secs = t[0] * 3600 + t[1] * 60 + t[2];
    Some(if secs == 0 { days.to_string() } else { format!("{}", days as f64 + secs as f64 / 86400.0) })
}

/// Фейк для значения из кэша сводной: поле `col` листа-источника, строка `row` (для записей).
fn pivot_lookup(rules: &[&Rule], row: Option<u32>, col: u32, raw: &str, ty: &[u8], faker: &Faker, d1904: bool) -> Option<String> {
    let orig = if ty == b"d" { iso_to_serial(raw, d1904)? } else { raw.to_string() };
    for r in rules.iter().filter(|r| r.col == col && row.map_or(true, |x| r.covers(x, col))) {
        if let Some(f) = faker.get(&r.kind_key(), &orig) {
            return match ty {
                b"d" => serial_to_iso(f, d1904),
                b"n" => is_plain_number(f).then(|| f.to_string()),
                _ => Some(f.to_string()),
            };
        }
    }
    None
}

fn with_attr(e: &BytesStart, key: &[u8], value: Option<&str>) -> BytesStart<'static> {
    let mut n = copy_without(e, key);
    if let Some(v) = value {
        n.push_attribute((String::from_utf8_lossy(key).as_ref(), v));
    }
    n
}

fn rewrite_pivot(data: &[u8], is_rec: bool, pc: &PivotCache, rules: &[Rule], faker: &Faker, d1904: bool) -> Result<Option<Vec<u8>>> {
    match &pc.source {
        Some((sheet, c0, r0)) => {
            let rs: Vec<&Rule> = rules.iter().filter(|r| &r.sheet == sheet).collect();
            if rs.is_empty() {
                return Ok(None);
            }
            rewrite_pivot_items(data, is_rec, *c0, *r0, &rs, faker, d1904)
        }
        // Источник неизвестен: записи кэша очищаем, Excel пересоберёт сводную при открытии.
        None => rewrite_pivot_root(data, is_rec).map(Some),
    }
}

fn rewrite_pivot_root(data: &[u8], is_rec: bool) -> Result<Vec<u8>> {
    let mut rd = Reader::from_reader(data);
    let mut w = Writer::new(Vec::new());
    let mut buf = Vec::new();
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if is_rec => {
                w.write_event(Event::Empty(with_attr(e, b"count", Some("0"))))?;
                break;
            }
            Event::Start(e) if e.local_name().as_ref() == b"pivotCacheDefinition" => {
                let n = with_attr(&with_attr(e, b"refreshOnLoad", Some("1")), b"recordCount", Some("0"));
                w.write_event(Event::Start(n))?;
                continue;
            }
            _ => {}
        }
        w.write_event(ev)?;
    }
    Ok(w.into_inner())
}

/// Определение: общие элементы полей (sharedItems). Записи: значения по строкам источника.
fn rewrite_pivot_items(data: &[u8], is_rec: bool, c0: u32, r0: u32, rules: &[&Rule], faker: &Faker, d1904: bool) -> Result<Option<Vec<u8>>> {
    let ruled_cols: HashSet<u32> = rules.iter().map(|r| r.col).collect();
    let mut rd = Reader::from_reader(data);
    let mut w = Writer::new(Vec::with_capacity(data.len()));
    let mut buf = Vec::new();
    let mut changed = false;
    let (mut field, mut rec, mut k): (i64, i64, i64) = (-1, -1, -1);
    let (mut in_shared, mut depth_r) = (false, 0usize);
    loop {
        let ev = rd.read_event_into(&mut buf)?.into_owned();
        buf.clear();
        let mut replaced: Option<Event<'static>> = None;
        match &ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let is_start = matches!(ev, Event::Start(_));
                let ln = e.local_name().as_ref().to_vec();
                let wrap = |n: BytesStart<'static>| if is_start { Event::Start(n) } else { Event::Empty(n) };
                if is_rec {
                    if ln == b"r" && depth_r == 0 {
                        rec += 1;
                        k = -1;
                        if is_start {
                            depth_r = 1;
                        }
                    } else if depth_r > 0 {
                        if depth_r == 1 {
                            k += 1;
                            if matches!(ln.as_slice(), b"n" | b"s" | b"d") {
                                let row = r0 + 1 + rec as u32;
                                let col = c0 + k as u32;
                                if let Some(out) = attr(e, b"v").and_then(|v| pivot_lookup(rules, Some(row), col, &v, &ln, faker, d1904)) {
                                    replaced = Some(wrap(with_attr(e, b"v", Some(&out))));
                                }
                            }
                        }
                        if is_start {
                            depth_r += 1;
                        }
                    }
                } else {
                    match ln.as_slice() {
                        b"cacheField" => field += 1,
                        b"sharedItems" => {
                            in_shared = is_start;
                            let col = c0 + field.max(0) as u32;
                            if ruled_cols.contains(&col) {
                                let mut n = e.clone().into_owned();
                                for key in [&b"minValue"[..], b"maxValue", b"minDate", b"maxDate"] {
                                    n = with_attr(&n, key, None);
                                }
                                replaced = Some(wrap(n));
                            }
                        }
                        b"n" | b"s" | b"d" if in_shared => {
                            let col = c0 + field.max(0) as u32;
                            if let Some(out) = attr(e, b"v").and_then(|v| pivot_lookup(rules, None, col, &v, &ln, faker, d1904)) {
                                replaced = Some(wrap(with_attr(e, b"v", Some(&out))));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                if is_rec && depth_r > 0 {
                    depth_r -= 1;
                } else if e.local_name().as_ref() == b"sharedItems" {
                    in_shared = false;
                }
            }
            _ => {}
        }
        match replaced {
            Some(n) => {
                changed = true;
                w.write_event(n)?;
            }
            None => w.write_event(ev)?,
        }
    }
    Ok(changed.then(|| w.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps() -> GlobalMaps {
        let mut m = GlobalMaps::default();
        m.strings.insert("Сидоров П.А.".into(), "Потапов С.В. (ID 2)".into());
        m.strings.insert("Иванов Иван Иванович".into(), "Петров Олег Сергеевич (ID 1)".into());
        m.build_inner();
        m
    }

    #[test]
    fn user_paths_masked_only_in_profile_folders() {
        assert_eq!(
            mask_user_paths(r#"<a url="C:\Users\petrov.p\Docs\" b='/Users/anna/x' c="file:///D:/USERS/ivan/y" d="file:///Users/olga/z"/>"#).unwrap(),
            r#"<a url="C:\Users\user\Docs\" b='/Users/user/x' c="file:///D:/USERS/user/y" d="file:///Users/user/z"/>"#
        );
        // Не профиль: папка с похожим началом, адрес сайта, сетевой путь, уже обезличенный путь.
        for s in [r"C:\UsersData\petrov\x", r#"<a href="https://site.ru/users/ivan/"/>"#, r"\\srv\Users\ivan\x", r"C:\Users\user\x"] {
            assert_eq!(mask_user_paths(s), None, "{s}");
        }
    }

    #[test]
    fn formula_literals() {
        let m = maps();
        assert_eq!(
            replace_formula_literals(r#"SUMIF(A:A,"Сидоров П.А.",B:B)&"""x""""#, &m).unwrap(),
            r#"SUMIF(A:A,"Потапов С.В. (ID 2)",B:B)&"""x""""#
        );
        assert_eq!(replace_formula_literals(r#"COUNTIF(A:A,"Другой")"#, &m), None);
    }

    #[test]
    fn inner_text_by_word_boundary() {
        let m = maps();
        assert_eq!(
            m.replace_in("Ответственный: Иванов Иван Иванович, тел.").unwrap(),
            "Ответственный: Петров Олег Сергеевич (ID 1), тел."
        );
        assert_eq!(m.replace_in("Иванов Иван Ивановичев"), None);
    }

    #[test]
    fn serial_iso_roundtrip() {
        assert_eq!(serial_to_iso("30000", false).unwrap(), "1982-02-18T00:00:00");
        assert_eq!(iso_to_serial("1982-02-18T00:00:00", false).unwrap(), "30000");
        assert_eq!(serial_to_iso("45000.5", false).unwrap(), "2023-03-15T12:00:00");
        assert_eq!(serial_to_iso("30000", true).unwrap(), "1986-02-19T00:00:00");
        assert_eq!(iso_to_serial("1986-02-19T00:00:00", true).unwrap(), "30000");
        // Дробная часть почти 1 — следующий день, а не «T24:00:00».
        assert_eq!(serial_to_iso("30000.999999999", false).unwrap(), "1982-02-19T00:00:00");
    }
}

// ---------------------------------------------------------------- проверка

/// Как значение может выглядеть в XML: в тексте («&amp;», кавычки как есть), в атрибуте («&quot;»), полностью.
fn escape_variants(s: &str) -> Vec<String> {
    let text = s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let attr = text.replace('"', "&quot;");
    let full = attr.replace('\'', "&apos;");
    let mut v = vec![s.to_string(), text, attr, full];
    v.sort();
    v.dedup();
    v
}

/// Склеенный текст абзацев (общие строки, примечания, надписи): <si>, <is>, <text>, <a:p>.
fn paragraph_texts(xml: &[u8]) -> Vec<String> {
    let mut rd = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let (mut depth, mut in_t, mut cur) = (0usize, false, String::new());
    loop {
        let Ok(ev) = rd.read_event_into(&mut buf) else { break };
        match ev {
            Event::Eof => break,
            Event::Start(e) => match e.local_name().as_ref() {
                b"si" | b"is" | b"text" | b"p" => {
                    if depth == 0 {
                        cur.clear();
                    }
                    depth += 1;
                }
                b"t" if depth > 0 => in_t = true,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"si" | b"is" | b"text" | b"p" if depth > 0 => {
                    depth -= 1;
                    if depth == 0 && !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                b"t" => in_t = false,
                _ => {}
            },
            Event::Text(t) if in_t || depth > 0 => {
                if let Ok(x) = t.unescape() {
                    cur.push_str(&x);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Ищет реальные значения в XML результата. Значения, которые сами входят в какой-либо фейк
/// (например, имя «Иван» внутри фейка «Иванова»), не проверяются, чтобы не было ложных тревог.
pub fn scan_leaks(path: &Path, originals: &[String], fakes: &[&str]) -> Result<Vec<Leak>> {
    scan_leaks_with(path, originals, fakes, &HashSet::new())
}

/// То же, плюс номера по цифрам в любом написании (`digits` — реальные цифры номеров и 10 цифр телефонов)
/// и тексты ЗАГЛАВНЫМИ/строчными.
pub fn scan_leaks_with(path: &Path, originals: &[String], fakes: &[&str], digits: &HashSet<String>) -> Result<Vec<Leak>> {
    let mut cased: Vec<String> = originals.to_vec();
    for s in originals {
        if s.chars().any(char::is_alphabetic) {
            cased.push(s.to_uppercase());
            cased.push(s.to_lowercase());
        }
    }
    cased.sort();
    cased.dedup();
    let originals = &cased;
    let fakes_cased: Vec<String> = fakes.iter().flat_map(|f| [f.to_string(), f.to_uppercase(), f.to_lowercase()]).collect();
    let fakes: Vec<&str> = fakes_cased.iter().map(String::as_str).collect();
    let cands: Vec<&String> = originals
        .iter()
        .filter(|s| {
            let n = s.chars().count();
            let numeric = s.trim().parse::<f64>().is_ok();
            (numeric && n >= 9) || (!numeric && n >= 4)
        })
        .collect();
    if cands.is_empty() {
        return Ok(Vec::new());
    }
    let ac = AhoCorasick::new(cands.iter().map(|s| s.as_str()))?;
    let mut excluded = vec![false; cands.len()];
    for &f in &fakes {
        for m in ac.find_overlapping_iter(f) {
            excluded[m.pattern().as_usize()] = true;
        }
    }
    let mut pats: Vec<String> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, s) in cands.iter().enumerate() {
        if excluded[i] {
            continue;
        }
        for v in escape_variants(s) {
            pats.push(v);
            owner.push(i);
        }
    }
    if pats.is_empty() && digits.is_empty() {
        return Ok(Vec::new());
    }
    let ac = AhoCorasick::new(&pats)?;
    let mut zip = open_zip(path)?;
    let mut leaks = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name().to_string();
        if !(name.ends_with(".xml") || name.ends_with(".rels") || name.ends_with(".vml")) {
            continue;
        }
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        let mut count = 0;
        let mut samples: Vec<String> = Vec::new();
        let note = |m: aho_corasick::Match, samples: &mut Vec<String>| {
            let s = cands[owner[m.pattern().as_usize()]];
            if samples.len() < 8 && !samples.contains(s) {
                samples.push(s.clone());
            }
        };
        for m in ac.find_iter(&data) {
            count += 1;
            note(m, &mut samples);
        }
        // Текст, разрезанный на фрагменты форматирования, в сыром XML не виден — ищем и в склеенном.
        let mut joined_hits = 0;
        for text in paragraph_texts(&data) {
            for m in ac.find_iter(&text) {
                joined_hits += 1;
                note(m, &mut samples);
            }
        }
        count = count.max(joined_hits);
        if !digits.is_empty() {
            let text = String::from_utf8_lossy(&data);
            for (a, b) in digit_runs(&text) {
                let run = &text[a..b];
                let d = digits_only(run);
                let hit = |d: &str| digits.contains(d) || (d.len() == 11 && (d.starts_with('7') || d.starts_with('8')) && digits.contains(&d[1..]));
                let found = if hit(&d) { Some(run.to_string()) } else { digit_groups(&text, a, b).into_iter().map(|(x, y)| &text[x..y]).find(|g| hit(g)).map(String::from) };
                if let Some(sample) = found {
                    count += 1;
                    if samples.len() < 8 && !samples.contains(&sample) {
                        samples.push(sample);
                    }
                }
            }
        }
        if count > 0 {
            leaks.push(Leak { part: name, count, samples });
        }
    }
    Ok(leaks)
}
