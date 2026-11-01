//! 0.6.0: две дыры приватности из аудита 0.5.
//! 1. «Название колонки» — строка шапки (больше всего текста в первых 50 строках). Над таблицей бывает
//!    строка-карточка «Иванов Иван Иванович | Менеджер | …» не под правилами: её ячейка становилась «колонкой
//!    «Иванов Иван Иванович»» в журнале (его пересылают), в предупреждениях и на карточке правила.
//! 2. Профиль хранит имена листов и подписи шаблонов как есть: лист, названный по сотруднику, делает профиль
//!    непригодным для пересылки — теперь об этом предупреждение (маской).
//! Каждый тест падал до исправления.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{preview, preview_batch, profile_risks, run, run_batch, Batch, FileJob, Job, Report};
use finboard_anon::profile::{privacy_risks, Kind, Profile, Rule, Settings};

fn settings() -> Settings {
    Settings { salt: "p6".into(), ..Settings::default() }
}

fn profile(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: settings() }
}

fn go(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, rules: Vec<Rule>) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile: profile(rules) };
    run(&job, &mut |_| {}).unwrap()
}

/// Всё, что уходит в журнал: строки отчёта и предупреждения.
fn journal(rep: &Report, rules: Vec<Rule>) -> String {
    let mut l = rep.summary(&profile(rules));
    l.extend(rep.warnings.iter().cloned());
    l.join("\n")
}

/// Части реального значения, которых не должно быть в журнале: слова от трёх букв и номера.
fn pieces(real: &str) -> Vec<String> {
    real.split(|c: char| !c.is_alphanumeric() && c != '-')
        .map(|w| w.trim_matches('-'))
        .filter(|w| w.chars().count() >= 3 && !["ООО", "ИНН", "КПП"].contains(w))
        // Короткие числа («916») бывают и в путях, и в счётчиках — в журнале они не улика.
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()) || w.len() >= 5)
        .map(String::from)
        .collect()
}

/// Лист: строка-карточка (больше всего текста — её сочтут шапкой), потом таблица «Ответственный | Покупатель».
/// В таблице одно значение размечено и как ФИО, и как организация — предупреждение называет колонки.
fn card_book(path: &Path, card: &[&str]) {
    let mut b = Book::default();
    let sh = b.sheet("Л");
    for (c, t) in card.iter().enumerate() {
        sh.push((1, c as u32, s(t)));
    }
    sh.extend([(3, 0, s("Ответственный")), (3, 1, s("Покупатель"))]);
    sh.extend([(4, 0, s("Петров Пётр")), (4, 1, s("ООО «Лютик»"))]);
    sh.extend([(5, 0, s("Кузнецов Олег")), (5, 1, s("Кузнецов Олег"))]);
    write_xlsx(path, &b);
}

fn card_rules() -> Vec<Rule> {
    vec![Rule::new("Л", 0, 4, None, Kind::FullName), Rule::new("Л", 1, 4, None, Kind::Company)]
}

/// Карточка над таблицей с ФИО, ИНН, организацией, телефоном, почтой: ни одно из них не становится названием
/// колонки — ни в отчёте, ни в предупреждении о разных типах, ни в предпросмотре (карточка правила в окне).
#[test]
fn p01_card_row_is_never_a_column_name() {
    let d = tmp("p01");
    let input = d.join("in.xlsx");
    for real in ["Иванов Иван Иванович", "Сидорова Анна Петровна", "Петров И.И.", "ИНН 7707083893", "ООО «Ромашка-Трейд»", "+7 (916) 111-22-33", "ivanov@mail.ru"] {
        card_book(&input, &[real, "Менеджер", "Отдел продаж"]);
        let rep = go(&input, &d.join("out.xlsx"), None, None, card_rules());
        let text = journal(&rep, card_rules());
        assert!(text.contains("разные фейки"), "{real}: нет предупреждения о типах:\n{text}");
        for p in pieces(real) {
            assert!(!text.contains(&p), "{real}: в журнале «{p}»:\n{text}");
        }
        assert_eq!(rep.rule_heads[0], None, "{real}");
        let pv = preview(&input, &profile(card_rules()), None).unwrap();
        assert_eq!(pv[0].head, None, "{real}: на карточке правила");
    }
}

/// Заголовок совпадает со значением под правилом другого файла прогона (без формы и не похож на ФИО) —
/// раньше сверялось только со своим файлом.
#[test]
fn p02_card_row_value_from_other_file_hidden() {
    let d = tmp("p02");
    let (a, b) = (d.join("Отчет.xlsx"), d.join("Контрагенты.xlsx"));
    card_book(&a, &["Вектор Плюс", "Склад", "Период"]);
    let mut bk = Book::default();
    bk.sheet("Л").extend([(1, 0, s("Контрагент")), (1, 1, s("Город")), (2, 0, s("Вектор Плюс")), (2, 1, s("Тверь"))]);
    write_xlsx(&b, &bk);
    let batch = Batch {
        files: vec![
            FileJob { input: a.clone(), output: d.join("o1.xlsx"), rules: card_rules() },
            FileJob { input: b.clone(), output: d.join("o2.xlsx"), rules: vec![Rule::new("Л", 0, 2, None, Kind::Company)] },
        ],
        settings: settings(),
        key_in: None,
        key_out: None,
    };
    let reps = run_batch(&batch, &mut |_| {}).unwrap();
    let text = journal(&reps[0], card_rules());
    assert!(!text.contains("Вектор"), "{text}");
    assert!(text.contains("колонка A (Отчет.xlsx, лист Л)"), "колонка буквой:\n{text}");
    let pv = preview_batch(&[(a, card_rules()), (b, vec![Rule::new("Л", 0, 2, None, Kind::Company)])], &settings(), None).unwrap();
    assert_eq!(pv[0][0].head, None);
}

/// Заголовок совпадает со значением ключа прошлых запусков (любого типа) — тоже не название колонки.
#[test]
fn p03_card_row_value_from_key_hidden() {
    let d = tmp("p03");
    let key = d.join("ключ.csv");
    let (a, b) = (d.join("Отчет.xlsx"), d.join("Контрагенты.xlsx"));
    let mut bk = Book::default();
    bk.sheet("Л").extend([(1, 0, s("Склад")), (1, 1, s("Город")), (2, 0, s("Северный Гранит")), (2, 1, s("Тверь")), (3, 0, s("Опт")), (3, 1, s("Тверь"))]);
    write_xlsx(&b, &bk);
    go(&b, &d.join("o2.xlsx"), None, Some(&key), vec![Rule::new("Л", 0, 2, None, Kind::Text)]);
    card_book(&a, &["Северный Гранит", "Склад", "Период"]);
    let rep = go(&a, &d.join("o1.xlsx"), Some(&key), None, card_rules());
    let text = journal(&rep, card_rules());
    assert!(!text.contains("Гранит"), "{text}");
    let pv = preview(&a, &profile(card_rules()), Some(&key)).unwrap();
    assert_eq!(pv[0].head, None);
}

/// Обычные заголовки по-прежнему называют колонки — и с ключом, и с категориями «Шаблона» в других колонках
/// («Розница» в данных не прячет заголовок «Розница цена»).
#[test]
fn p04_normal_headers_still_named() {
    let d = tmp("p04");
    let key = d.join("ключ.csv");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Л").extend([
        (1, 0, s("Продажи за период")),
        (3, 0, s("Ответственный")),
        (3, 1, s("Покупатель группа")),
        (3, 2, s("Канал")),
        (3, 3, s("Розница цена")),
        (3, 4, s("Остаток на 01.10.2024")),
        (4, 0, s("Иванов Иван")),
        (4, 1, s("ООО «Ромашка»")),
        (4, 2, s("Розница")),
        (4, 3, s("Дорого")),
        (4, 4, s("Мало")),
        (5, 0, s("Петров Пётр")),
        (5, 1, s("ООО «Лютик»")),
        (5, 2, s("Опт")),
        (5, 3, s("Дёшево")),
        (5, 4, s("Много")),
    ]);
    write_xlsx(&input, &b);
    let rules = || {
        vec![
            Rule::new("Л", 0, 4, None, Kind::FullName),
            Rule::new("Л", 1, 4, None, Kind::Company),
            Rule::new("Л", 2, 4, None, Kind::Text),
            Rule::new("Л", 3, 4, None, Kind::Text),
            Rule::new("Л", 4, 4, None, Kind::Text),
        ]
    };
    go(&input, &d.join("o0.xlsx"), None, Some(&key), rules());
    let rep = go(&input, &d.join("out.xlsx"), Some(&key), None, rules());
    let text = journal(&rep, rules());
    for h in ["Ответственный", "Покупатель группа", "Канал", "Розница цена", "Остаток на 01.10.2024"] {
        assert!(text.contains(&format!("колонка «{h}», правило")), "нет «{h}»:\n{text}");
    }
    let pv = preview(&input, &profile(rules()), Some(&key)).unwrap();
    assert_eq!(pv[0].head.as_deref(), Some("Ответственный"));
    assert_eq!(pv[1].head.as_deref(), Some("Покупатель группа"));
    // Профиль чистый: предупреждений о листах и подписях нет.
    assert!(!text.contains("в профиле"), "{text}");
    assert!(pv.iter().all(|p| p.risks.is_empty()), "{:?}", pv.iter().map(|p| &p.risks).collect::<Vec<_>>());
}

/// `privacy_risks`: имя листа или подпись шаблона, совпавшие со значением (в том числе фамилией из ФИО и
/// названием организации без формы) или похожие на ФИО/номер, — предупреждение маской; чистый профиль — без него.
#[test]
fn p05_profile_names_like_data_warned_masked() {
    let values: Vec<(&str, &str, Option<Kind>)> = vec![
        ("Сидорова Анна Петровна", "колонка A, in.xlsx", Some(Kind::FullName)),
        ("ООО «Ромашка»", "колонка B, in.xlsx", Some(Kind::Company)),
        ("Должность 7", "колонка C, in.xlsx", Some(Kind::Text)),
        ("Москва", "ключ прошлых запусков", Some(Kind::Address)),
    ];
    let clean = profile(vec![Rule::new("Продажи", 0, 2, None, Kind::FullName), {
        let mut r = Rule::new("Продажи", 2, 2, None, Kind::Text);
        r.label = "Должность".into();
        r
    }]);
    assert!(privacy_risks(&clean, values.iter().copied()).is_empty(), "{:?}", privacy_risks(&clean, values.iter().copied()));

    for (sheet, real_piece, why) in [
        ("Иванов Иван", "Иванов", "похоже на ФИО"),
        ("Сидорова", "Сидорова", "колонка A, in.xlsx"),
        ("Ромашка", "Ромашка", "колонка B, in.xlsx"),
        ("Москва", "Москва", "ключ прошлых запусков"),
        ("7707083893", "7707083893", "похоже на номер"),
    ] {
        let p = profile(vec![Rule::new(sheet, 0, 2, None, Kind::FullName), Rule::new(sheet, 1, 2, None, Kind::Company)]);
        let w = privacy_risks(&p, values.iter().copied());
        assert_eq!(w.len(), 1, "{sheet}: одна строка на лист: {w:?}");
        assert!(w[0].contains("имя листа «") && w[0].contains("переименуйте лист или не пересылайте профиль"), "{w:?}");
        assert!(w[0].contains(why), "{sheet}: {w:?}");
        assert!(!w[0].contains(real_piece), "{sheet}: значение не маской: {w:?}");
    }

    let mut t = Rule::new("Продажи", 2, 2, None, Kind::Text);
    t.label = "Сидорова Анна".into();
    let w = privacy_risks(&profile(vec![t]), values.iter().copied());
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("подпись шаблона «С*** А***»") && w[0].contains("обезличенный файл"), "{w:?}");
    assert!(!w[0].contains("Сидорова"), "{w:?}");
}

/// Обработка (журнал окна при сохранении профиля рядом с файлом, командная строка) и проверка перед
/// «Сохранить профиль…»: лист назван по сотруднику из данных — предупреждение маской; чистый — нет.
#[test]
fn p06_run_and_save_warn_about_sheet_named_like_value() {
    let d = tmp("p06");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Кузнецов").extend([(1, 0, s("Ответственный")), (1, 1, s("Сумма")), (2, 0, s("Кузнецов Олег Ильич")), (2, 1, Cell::N(5.0))]);
    b.sheet("Продажи").extend([(1, 0, s("Ответственный")), (1, 1, s("Сумма")), (2, 0, s("Петров Пётр")), (2, 1, Cell::N(7.0))]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Кузнецов", 0, 2, None, Kind::FullName), Rule::new("Продажи", 0, 2, None, Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), None, None, rules());
    let w: Vec<&String> = rep.warnings.iter().filter(|w| w.contains("в профиле")).collect();
    assert_eq!(w.len(), 1, "{:?}", rep.warnings);
    assert!(w[0].contains("имя листа «К***»") && w[0].contains("колонка A, in.xlsx"), "{w:?}");
    assert!(rep.summary(&profile(rules())).iter().any(|l| l.starts_with("Внимание: в профиле имя листа «К***»")));
    let pv = preview(&input, &profile(rules()), None).unwrap();
    assert_eq!(pv[0].risks.len(), 1, "{:?}", pv[0].risks);
    assert!(pv[1].risks.is_empty(), "{:?}", pv[1].risks);

    let files: Vec<(PathBuf, Vec<Rule>)> = vec![(input.clone(), rules())];
    let r = profile_risks(&profile(rules()), &files, None).unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(!r[0].contains("Кузнецов"), "{r:?}");
    let only_clean = vec![Rule::new("Продажи", 0, 2, None, Kind::FullName)];
    assert!(profile_risks(&profile(only_clean.clone()), &[(input.clone(), only_clean.clone())], None).unwrap().is_empty());
    let rep = go(&input, &d.join("out2.xlsx"), None, None, only_clean);
    assert!(!rep.warnings.iter().any(|w| w.contains("в профиле")), "{:?}", rep.warnings);
}
