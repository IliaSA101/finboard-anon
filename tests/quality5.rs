//! Качество данных 0.5.0: фейковые номера, адреса и организации не сливают разных контрагентов,
//! журнал не показывает реальных значений, ключ 0.4.0 воспроизводит прежний результат.
//! Тесты — на свойства (взаимная однозначность, отсутствие реальных значений), а не на отдельные примеры.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use common::*;
use finboard_anon::anon::{mask_sample, run, Job, Report};
use finboard_anon::fake::{phone_key, Faker, KeyEntry};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

fn go_log(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, rules: Vec<Rule>, s: Settings) -> (Report, Vec<String>) {
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile: Profile { rules, settings: s } };
    let mut log = Vec::new();
    let rep = run(&job, &mut |m| log.push(m)).unwrap();
    (rep, log)
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

fn rule_at(col: u32, kind: Kind, with_id: bool) -> Rule {
    let mut r = Rule::new("Л", col, 2, None, kind);
    r.with_id = with_id;
    r
}

// <v04-input> — тот же код собирает книгу и в 0.4.0 (для файлов tests/data/q5_v04_*), не менять.

/// Детерминированный поток «случайных» чисел для тестовых данных.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// n цифр, первая не ноль.
    fn digits(&mut self, n: usize) -> String {
        (0..n).map(|i| char::from(b'0' + if i == 0 { 1 + self.next() % 9 } else { self.next() % 10 } as u8)).collect()
    }
}

/// Колонки: A — ИНН без ID, B — телефон без ID, C — короткий код без ID, D — адрес без ID,
/// E — ИНН-12 числом с ID, F — организация с ID (одно название с разной формой).
fn v04_book() -> Book {
    let mut r = Lcg(0x0405);
    let mut b = Book::default();
    let sh = b.sheet("Л");
    for (c, h) in ["ИНН", "Телефон", "Договор", "Адрес", "ИНН ИП", "Контрагент"].iter().enumerate() {
        sh.push((1, c as u32, s(h)));
    }
    let mut seen = HashSet::new();
    for row in 2..1002u32 {
        let inn = loop {
            let v = r.digits(10);
            if seen.insert(v.clone()) {
                break v;
            }
        };
        sh.push((row, 0, s(&inn)));
        let p = loop {
            let v = format!("9{}", r.digits(9));
            if seen.insert(v.clone()) {
                break v;
            }
        };
        sh.push((row, 1, s(&format!("8 ({}) {}-{}-{}", &p[..3], &p[3..6], &p[6..8], &p[8..]))));
        sh.push((row, 3, s(&format!("г. Город{}, ул. Улица{}, д. {}", r.next() % 50, r.next() % 50, 1 + row))));
        if row < 502 {
            let code = loop {
                let v = format!("Дог{}", 1 + r.next() % 9999);
                if seen.insert(v.clone()) {
                    break v;
                }
            };
            sh.push((row, 2, s(&code)));
        }
        if row < 302 {
            let inn12 = loop {
                let v = r.digits(12);
                if seen.insert(v.clone()) {
                    break v;
                }
            };
            sh.push((row, 4, Cell::N(inn12.parse().unwrap())));
        }
        if row < 102 {
            let form = ["ООО", "АО", "Общество с ограниченной ответственностью"][(row % 3) as usize];
            sh.push((row, 5, s(&format!("{form} «Фирма{}»", row % 40))));
        }
    }
    b
}

fn v04_rules() -> Vec<Rule> {
    let rule = |col: u32, kind: Kind, with_id: bool| {
        let mut r = Rule::new("Л", col, 2, None, kind);
        r.with_id = with_id;
        r
    };
    vec![
        rule(0, Kind::Inn, false),
        rule(1, Kind::Phone, false),
        rule(2, Kind::Account, false),
        rule(3, Kind::Address, false),
        rule(4, Kind::Inn, true),
        rule(5, Kind::Company, true),
    ]
}

/// Ячейки листа «Л» результата: «A2» → текст.
fn out_cells(path: &Path) -> BTreeMap<String, String> {
    let g = read_book(path);
    g["Л"]
        .cells
        .iter()
        .map(|((r, c), v)| (format!("{}{}", finboard_anon::xlsx::col_name(*c), r), v.text.clone()))
        .collect()
}

// </v04-input>

fn read_tsv(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, v)| (a.to_string(), v.replace("\\n", "\n")))
        .collect()
}

fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data")
}

// ---------------------------------------------------------------- 1. номера: разным реальным — разные фейки

/// Тысячи случайных номеров каждого вида и длины (ИНН 10/12, СНИЛС, паспорт, КПП, ОГРН, карта, счёт,
/// телефоны, короткие коды) — с ID и без: ни у каких двух разных номеров нет общего фейка, и ни один фейк
/// не совпадает с настоящим номером. В 0.4.0 без ID ИНН брал пять случайных цифр — при нескольких сотнях
/// ИНН совпадения почти неизбежны (парадокс дней рождения), разные контрагенты сливались.
#[test]
fn numbers_one_to_one_for_every_kind() {
    // (тип, длина, сколько)
    let plan: [(Kind, usize, usize); 13] = [
        (Kind::Inn, 10, 3000),
        (Kind::Inn, 12, 3000),
        (Kind::Snils, 11, 3000),
        (Kind::Passport, 10, 3000),
        (Kind::Account, 5, 3000),
        (Kind::Account, 9, 3000),  // КПП
        (Kind::Account, 13, 3000), // ОГРН
        (Kind::Account, 16, 3000), // карта
        (Kind::Account, 20, 3000), // расчётный счёт
        (Kind::Phone, 11, 3000),
        (Kind::Phone, 10, 3000),
        (Kind::Phone, 6, 3000),
        (Kind::Account, 0, 3000), // короткие коды «Дог1»
    ];
    for with_id in [false, true] {
        let mut r = Lcg(if with_id { 7 } else { 11 });
        let mut seen = HashSet::new();
        let mut values: Vec<(Kind, usize, String)> = Vec::new();
        for (kind, len, n) in plan {
            let mut k = 0;
            while k < n {
                let v = match (kind, len) {
                    (_, 0) => format!("Дог{}", 1 + r.next() % 9999),
                    (Kind::Phone, 11) => {
                        let d = format!("9{}", r.digits(9));
                        format!("8 ({}) {}-{}-{}", &d[..3], &d[3..6], &d[6..8], &d[8..])
                    }
                    (Kind::Phone, 10) => format!("9{}", r.digits(9)),
                    _ => r.digits(len),
                };
                let id = if kind == Kind::Phone { phone_key(&v) } else if len == 0 { v.to_lowercase() } else { digits(&v) };
                if seen.insert(id) {
                    values.push((kind, len, v));
                    k += 1;
                }
            }
        }
        let mut f = Faker::new(&settings("q5"));
        f.note_reals(values.iter().map(|(_, _, v)| v.as_str()));
        let mut groups: HashMap<(Kind, usize), Vec<(String, String)>> = HashMap::new();
        for (kind, len, v) in &values {
            let fake = f.fake(&rule_at(0, *kind, with_id), v, false);
            groups.entry((*kind, *len)).or_default().push((v.clone(), fake));
        }
        let norm = |kind: Kind, len: usize, s: &str| {
            if kind == Kind::Phone {
                phone_key(&digits(s))
            } else if len == 0 {
                s.to_lowercase()
            } else {
                digits(s)
            }
        };
        let reals: HashSet<String> = values.iter().map(|(k, l, v)| norm(*k, *l, v)).collect();
        // Все номера вместе: ИНН, паспорт и телефон из одних цифр — тоже не один фейк.
        let mut owner: HashMap<String, String> = HashMap::new();
        for ((kind, len), pairs) in &groups {
            for (real, fake) in pairs {
                let (kr, kf) = (norm(*kind, *len, real), norm(*kind, *len, fake));
                assert!(!reals.contains(&kf), "{kind:?}/{len} с ID={with_id}: фейк {fake} совпал с настоящим значением");
                if *len > 0 {
                    assert_eq!(digits(fake).len(), digits(real).len(), "{kind:?}: длина номера сохраняется: {real} → {fake}");
                }
                if let Some(prev) = owner.insert(kf.clone(), kr.clone()) {
                    assert_eq!(prev, kr, "{kind:?}/{len} с ID={with_id}: {prev} и {kr} получили один фейк {fake}");
                }
            }
        }
        // Повтор того же значения — тот же фейк (детерминированность не потеряна).
        for ((kind, _), pairs) in &groups {
            for (real, fake) in pairs.iter().step_by(97) {
                assert_eq!(&f.fake(&rule_at(0, *kind, with_id), real, false), fake);
            }
        }
    }
}

/// Тот же прогон на новом генераторе с той же солью — те же фейки: выбор не зависит от случайности запуска.
#[test]
fn numbers_deterministic_across_runs() {
    let mut r = Lcg(99);
    let vals: Vec<String> = (0..2000).map(|_| r.digits(10)).collect();
    let run = || {
        let mut f = Faker::new(&settings("q5"));
        vals.iter().map(|v| f.fake(&rule_at(0, Kind::Inn, false), v, false)).collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

/// ID шире места под него в номере: в 0.4.0 брались последние цифры — ID 100001 давал тот же ИНН, что ID 1.
/// Теперь ID занимает и соседние «нулевые» разряды (номер по-прежнему виден), а где места нет совсем —
/// номер берётся свободный.
#[test]
fn ids_above_width_do_not_wrap() {
    // (тип, длина реального номера, ID, на котором кончается место в 0.4.0, ожидаемое начало фейка для этого ID)
    let cases: [(Kind, usize, u32, Option<&str>); 7] = [
        (Kind::Inn, 10, 100_000, Some("990100000")),
        (Kind::Inn, 12, 1_000_000, Some("9901000000")),
        (Kind::Passport, 10, 1_000_000, Some("9901000000")),
        (Kind::Phone, 11, 10_000_000, Some("89010000000")),
        (Kind::Snils, 11, 900_000_000, None),
        (Kind::Account, 5, 90_007, None),
        (Kind::Account, 9, 1_000_000_000, None),
    ];
    for (kind, len, boundary, expect) in cases {
        let rule = rule_at(0, kind, true);
        let mut r = Lcg(boundary as u64);
        let mut gen = || if kind == Kind::Phone { format!("8{}", r.digits(10)) } else { r.digits(len) };
        let mut f = Faker::new(&settings("q5"));
        let mut fakes: Vec<(String, String)> = Vec::new();
        for _ in 0..8 {
            let v = gen();
            fakes.push((v.clone(), f.fake(&rule, &v, false)));
        }
        if kind == Kind::Inn && len == 10 {
            assert!(digits(&fakes[0].1).starts_with("990000001"), "ID 1 — как раньше: {}", fakes[0].1);
        }
        // Ключ прошлых запусков с ID у самой границы: следующие номера получают ID за ней.
        let (dummy, dummy_fake) = (gen(), gen());
        f.load_entries(vec![KeyEntry { kind_key: rule.kind_key(), id: Some(boundary - 1), real: dummy.clone(), fake: dummy_fake.clone() }]);
        fakes.push((dummy, dummy_fake));
        for i in 0..3 {
            let v = gen();
            let fake = f.fake(&rule, &v, false);
            if i == 0 {
                if let Some(p) = expect {
                    assert!(digits(&fake).starts_with(p), "{kind:?}: ID {boundary} виден в номере: {fake}");
                }
            }
            fakes.push((v, fake));
        }
        let mut owner = HashMap::new();
        for (real, fake) in &fakes {
            let k = if kind == Kind::Phone { phone_key(&digits(fake)) } else { digits(fake) };
            if let Some(prev) = owner.insert(k, real.clone()) {
                panic!("{kind:?}/{len}: {prev} и {real} получили один фейк {fake} (ID за границей {boundary})");
            }
        }
    }
}

// ---------------------------------------------------------------- 2. адреса и организации

/// Адреса без ID: город × улица × дом — около ста тысяч вариантов, среди тысяч адресов совпадения были.
#[test]
fn addresses_and_companies_one_to_one() {
    let mut f = Faker::new(&settings("q5"));
    let addr = rule_at(0, Kind::Address, false);
    let comp = rule_at(0, Kind::Company, false);
    let mut by_fake: HashMap<String, String> = HashMap::new();
    for i in 0..4000 {
        let a = format!("г. Город{}, ул. Улица{}, д. {}, кв. {}", i % 37, i % 41, i % 13, i);
        let fake = f.fake(&addr, &a, false);
        assert_ne!(fake.to_lowercase(), a.to_lowercase());
        if let Some(prev) = by_fake.insert(fake.to_lowercase(), a.clone()) {
            panic!("адреса {prev} и {a} получили один фейк {fake}");
        }
    }
    let mut by_fake: HashMap<String, String> = HashMap::new();
    for i in 0..3000 {
        let c = format!("ООО «Фирма{i}»");
        let fake = f.fake(&comp, &c, false);
        if let Some(prev) = by_fake.insert(fake.to_lowercase(), c.clone()) {
            panic!("организации {prev} и {c} получили один фейк {fake}");
        }
    }
}

/// «ООО «Ромашка»» и «АО «Ромашка»» — разные юрлица: в 0.4.0 ключ был названием без формы, и они
/// становились одной фейковой организацией с одним ID. Полная форма — то же, что сокращение. Название без
/// формы: если в данных у него одна форма — та же организация (так в выгрузках 1С: «Ромашка» в одной колонке,
/// «ООО «Ромашка»» в другой); если форм несколько — отдельная, чтобы не приписать данные чужому юрлицу.
#[test]
fn company_form_is_part_of_identity() {
    let d = tmp("q5-forms");
    let (input, output) = (d.join("in.xlsx"), d.join("out.xlsx"));
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("Контрагент")));
    sh.push((1, 1, s("Назначение")));
    let names = [
        "ООО «Ромашка»",
        "АО «Ромашка»",
        "Общество с ограниченной ответственностью «Ромашка»",
        "Ромашка",
        "ООО «Вектор»",
        "Вектор",
        "Акционерное общество \"Вектор-Строй\"",
        "АО «Вектор-Строй»",
        "ПАО «Вектор-Строй»",
    ];
    for (i, n) in names.iter().enumerate() {
        sh.push((2 + i as u32, 0, s(n)));
    }
    sh.push((2, 1, s("Оплата АО «Ромашка» по счёту 5")));
    write_xlsx(&input, &b);
    go_log(&input, &output, None, None, vec![rule_at(0, Kind::Company, true)], settings("q5"));
    let g = read_book(&output);
    let out: Vec<String> = (0..names.len()).map(|i| text(&g, "Л", &format!("A{}", 2 + i))).collect();
    let name = |s: &str| finboard_anon::fake::split_company(finboard_anon::fake::strip_id_suffix(s)).1;
    let id = |s: &str| s.rsplit_once("(ID ").map(|(_, x)| x.trim_end_matches(')').to_string()).unwrap_or_default();
    // ООО = полная форма ООО: одно название, один ID.
    assert_eq!((name(&out[0]), id(&out[0])), (name(&out[2]), id(&out[2])), "{out:?}");
    assert!(out[2].starts_with("Общество с ограниченной ответственностью «"), "{out:?}");
    // АО — другое юрлицо.
    assert_ne!(name(&out[0]), name(&out[1]), "{out:?}");
    assert_ne!(id(&out[0]), id(&out[1]), "{out:?}");
    // «Ромашка» без формы при двух формах — ни одна из них.
    assert!(name(&out[3]) != name(&out[0]) && name(&out[3]) != name(&out[1]), "{out:?}");
    // «Вектор» с единственной формой — та же организация.
    assert_eq!((name(&out[4]), id(&out[4])), (name(&out[5]), id(&out[5])), "{out:?}");
    // «Акционерное общество» ≡ АО, ПАО — другое.
    assert_eq!(name(&out[6]), name(&out[7]), "{out:?}");
    assert_ne!(name(&out[7]), name(&out[8]), "{out:?}");
    // Внутри текста — название того юрлица, что указано с формой.
    let note = text(&g, "Л", "B2");
    assert!(!note.contains("Ромашка"), "{note}");
    assert!(note.contains(&name(&out[1])), "АО «Ромашка» внутри текста — фейк АО: {note} / {out:?}");
}

// ---------------------------------------------------------------- 3. журнал без реальных значений

#[test]
fn mask_sample_shapes() {
    assert_eq!(mask_sample("Иванов Иван"), "И*** И***");
    assert_eq!(mask_sample("7707083893"), "*** 93");
    assert_eq!(mask_sample("+7 (916) 123-45-67"), "*** 67");
    assert_eq!(mask_sample("ООО «Ромашка»"), "О*** Р***");
    assert_eq!(mask_sample("ivan@corp.ru"), "i***");
    assert_eq!(mask_sample("12"), "***");
}

/// Журнал пересылают, а итог проверки печатал найденные остатки как есть — ФИО, ИНН, телефоны.
/// Инвариант: ни в одном сценарии утечки ни одна строка итога не содержит реального значения (и его слова,
/// и его цифр) длиной от 4 символов; при этом где искать — часть книги, число вхождений, намёк — есть.
#[test]
fn summary_never_shows_real_values() {
    let reals = ["Сидоров Пётр Алексеевич", "7707083893", "+7 (916) 123-45-67", "ivan.petrov@corp.ru", "ООО «Ромашка»"];
    let planted = [
        "Звонил Сидоров Пётр Алексеевич",
        "ИНН контрагента 7707083893",
        "Тел. 8 916 123-45-67",
        "Пишите на ivan.petrov@corp.ru",
        "Оплата ООО «Ромашка» по счёту",
    ];
    let mut forbidden: Vec<String> = Vec::new();
    for r in reals {
        forbidden.push(r.to_lowercase());
        forbidden.extend(r.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| w.chars().count() >= 4));
        let d = digits(r);
        if d.len() >= 4 {
            for w in d.as_bytes().windows(4) {
                forbidden.push(String::from_utf8(w.to_vec()).unwrap());
            }
        }
    }
    let d = tmp("q5-journal");
    // Каждая утечка отдельно и все вместе.
    let mut scenarios: Vec<Vec<usize>> = (0..planted.len()).map(|i| vec![i]).collect();
    scenarios.push((0..planted.len()).collect());
    for (si, sc) in scenarios.iter().enumerate() {
        let (input, output) = (d.join(format!("in{si}.xlsx")), d.join(format!("out{si}.xlsx")));
        let mut b = Book::default();
        let sh = b.sheet("Л");
        for (c, v) in reals.iter().enumerate() {
            sh.push((1, c as u32, s(["ФИО", "ИНН", "Телефон", "Почта", "Контрагент"][c])));
            sh.push((2, c as u32, s(v)));
        }
        for (k, &i) in sc.iter().enumerate() {
            sh.push((3 + k as u32, 5, s(planted[i])));
        }
        write_xlsx(&input, &b);
        let rules = vec![
            rule_at(0, Kind::FullName, true),
            rule_at(1, Kind::Inn, true),
            rule_at(2, Kind::Phone, true),
            rule_at(3, Kind::Email, true),
            rule_at(4, Kind::Company, true),
        ];
        // Только целые ячейки: текст с вкраплениями остаётся — проверка его находит.
        let st = Settings { inner_text: false, ..settings("q5") };
        let (rep, log) = go_log(&input, &output, None, None, rules.clone(), st.clone());
        assert!(!rep.leaks.is_empty(), "сценарий {sc:?}: утечка не найдена");
        let lines = rep.summary(&Profile { rules, settings: st });
        let all = lines.iter().chain(&log).map(|l| l.to_lowercase()).collect::<Vec<_>>().join("\n");
        for f in &forbidden {
            assert!(!all.contains(f.as_str()), "сценарий {sc:?}: в журнале реальное «{f}»:\n{all}");
        }
        let leak_line = lines.iter().find(|l| l.contains("вхожд., например:")).unwrap_or_else(|| panic!("{lines:?}"));
        assert!(leak_line.contains("xl/sharedStrings.xml") && leak_line.contains("***"), "{leak_line}");
    }
}

// ---------------------------------------------------------------- 4. строка про новые значения

/// «Уникальных значений: 0» после загрузки ключа на 2021 строку путала: считались только новые.
#[test]
fn log_counts_new_values_beyond_key() {
    let d = tmp("q5-new");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.push((1, 0, s("ИНН")));
    for i in 0..5u32 {
        sh.push((2 + i, 0, s(&format!("770708389{i}"))));
    }
    write_xlsx(&input, &b);
    let key = d.join("key.csv");
    let rules = vec![rule_at(0, Kind::Inn, true)];
    let (_, log) = go_log(&input, &d.join("o1.xlsx"), None, Some(&key), rules.clone(), settings("q5"));
    assert!(log.iter().any(|l| l == "Новых значений (нет в ключе): 5"), "{log:?}");
    let (_, log) = go_log(&input, &d.join("o2.xlsx"), Some(&key), Some(&key), rules, settings("q5"));
    assert!(log.iter().any(|l| l == "Новых значений (нет в ключе): 0"), "{log:?}");
    assert!(!log.iter().any(|l| l.starts_with("Уникальных значений")), "{log:?}");
}

// ---------------------------------------------------------------- 5. ключ 0.4.0

/// Ключ, записанный версией 0.4.0 (tests/data/q5_v04_key.csv, книга — `v04_book`), воспроизводит её результат
/// ячейка в ячейку — и там, где 0.4.0 выдала двум номерам один фейк: что уже передано, не меняется.
#[test]
fn key_from_v04_reproduces_v04_output() {
    let d = tmp("q5-v04");
    let input = d.join("in.xlsx");
    write_xlsx(&input, &v04_book());
    let key = d.join("key.csv");
    std::fs::copy(data_dir().join("q5_v04_key.csv"), &key).unwrap();
    let out = d.join("out.xlsx");
    // Другая соль: всё должно прийти из ключа.
    go_log(&input, &out, Some(&key), None, v04_rules(), settings("другая соль"));
    let expected = read_tsv(&data_dir().join("q5_v04_out.tsv"));
    let got = out_cells(&out);
    assert_eq!(got.len(), expected.len());
    for (a, v) in &expected {
        assert_eq!(got.get(a), Some(v), "ячейка {a}");
    }
}

/// Без ключа, с той же солью, 0.5.0 даёт те же фейки, что 0.4.0, — кроме номеров и адресов, которые
/// в 0.4.0 совпали с чужими: они теперь свои. Набор данных такой, что совпадения в 0.4.0 были.
#[test]
fn v05_without_key_keeps_v04_fakes_except_collisions() {
    let d = tmp("q5-v04-nokey");
    let input = d.join("in.xlsx");
    write_xlsx(&input, &v04_book());
    let out = d.join("out.xlsx");
    go_log(&input, &out, None, None, v04_rules(), settings("q5"));
    let old = read_tsv(&data_dir().join("q5_v04_out.tsv"));
    let new = out_cells(&out);
    let mut collided = 0;
    for col in ["A", "B", "C", "D", "E"] {
        let cells = |m: &BTreeMap<String, String>| -> Vec<(String, String)> {
            m.iter().filter(|(a, _)| a.starts_with(col) && a[1..] != *"1").map(|(a, v)| (a.clone(), v.clone())).collect()
        };
        let (o, n) = (cells(&old), cells(&new));
        let mut count: HashMap<&str, usize> = HashMap::new();
        for (_, v) in &o {
            *count.entry(v.as_str()).or_default() += 1;
        }
        let dup: HashSet<&str> = count.iter().filter(|(_, c)| **c > 1).map(|(v, _)| *v).collect();
        collided += dup.len();
        assert_eq!(o.iter().map(|(a, _)| a).collect::<Vec<_>>(), n.iter().map(|(a, _)| a).collect::<Vec<_>>());
        // Измениться могут фейки, совпавшие в 0.4.0, и изредка — тот, чей вариант занял заново выбранный номер.
        let mut other = Vec::new();
        for ((a, ov), (_, nv)) in o.iter().zip(&n) {
            if ov != nv && !dup.contains(ov.as_str()) {
                other.push(format!("{a}: {ov} → {nv}"));
            }
        }
        assert!(other.len() <= dup.len(), "колонка {col}: изменились фейки без совпадения: {other:?}");
        let uniq: HashSet<&String> = n.iter().map(|(_, v)| v).collect();
        assert_eq!(uniq.len(), n.len(), "колонка {col}: в 0.5.0 у разных значений один фейк");
    }
    assert!(collided > 0, "в наборе нет совпадений 0.4.0 — тест ничего не доказывает");
}

/// Файлы tests/data/q5_v04_* записываются версией 0.4.0: этот же код копируется в её исходники
/// (участок между метками `v04-input`) и запускается там. Здесь — только для справки, не запускается.
#[test]
#[ignore]
fn write_v04_fixture() {
    let dir = std::env::var("Q5_OUT").expect("Q5_OUT — папка для файлов");
    let dir = Path::new(&dir);
    let input = dir.join("in.xlsx");
    write_xlsx(&input, &v04_book());
    let (out, key) = (dir.join("out.xlsx"), dir.join("q5_v04_key.csv"));
    let job = Job { input: input.clone(), output: out.clone(), key_in: None, key_out: Some(key), profile: Profile { rules: v04_rules(), settings: settings("q5") } };
    run(&job, &mut |_| {}).unwrap();
    let tsv: String = out_cells(&out).iter().map(|(a, v)| format!("{a}\t{}\n", v.replace('\n', "\\n"))).collect();
    std::fs::write(dir.join("q5_v04_out.tsv"), tsv).unwrap();
}
