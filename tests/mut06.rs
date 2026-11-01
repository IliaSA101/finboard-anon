//! Тесты по выжившим мутантам кода 0.6.0 (tools/mutate.py, «0.6 …»): названия колонок и профиль без
//! персональных данных (сравнение без регистра и ё, часть заголовка, пакет, ключ прошлых запусков),
//! реальные значения ключа не становятся фейками. Каждый тест падает на своём мутанте — то есть ловит
//! настоящую ошибку такого вида.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{preview, preview_batch, profile_risks, run, run_batch, Batch, FileJob, Job, Report};
use finboard_anon::fake::{norm_text, split_company, Faker, KeyEntry};
use finboard_anon::profile::{privacy_risks, rule_risks, Kind, Profile, Rule, Settings};

fn settings() -> Settings {
    Settings { salt: "m6".into(), ..Settings::default() }
}

fn profile(rules: Vec<Rule>) -> Profile {
    Profile { rules, settings: settings() }
}

fn go(input: &Path, output: &Path, key_in: Option<&Path>, key_out: Option<&Path>, rules: Vec<Rule>) -> Report {
    let job = Job { input: input.into(), output: output.into(), key_in: key_in.map(Into::into), key_out: key_out.map(Into::into), profile: profile(rules) };
    run(&job, &mut |_| {}).unwrap()
}

fn journal(rep: &Report, rules: Vec<Rule>) -> String {
    let mut l = rep.summary(&profile(rules));
    l.extend(rep.warnings.iter().cloned());
    l.join("\n")
}

/// Лист `sheet`: шапка «Ответственный | Сумма», под ней ФИО `people`.
fn people_book(path: &Path, sheet: &str, people: &[&str]) {
    let mut b = Book::default();
    let sh = b.sheet(sheet);
    sh.extend([(1, 0, s("Ответственный")), (1, 1, s("Сумма"))]);
    for (i, p) in people.iter().enumerate() {
        sh.extend([(2 + i as u32, 0, s(p)), (2 + i as u32, 1, Cell::N(i as f64 + 1.0))]);
    }
    write_xlsx(path, &b);
}

/// Имя листа сравнивается со значениями без учёта регистра и ё/е: «Семёнов» и «СЕМЕНОВ» при «Семенов Олег
/// Ильич» — тот же человек. Мутанты «0.6 приватность: … (ё)» и «… (регистр)».
#[test]
fn m01_sheet_name_matched_ignoring_case_and_yo() {
    let values: Vec<(&str, &str, Option<Kind>)> = vec![
        ("Семенов Олег Ильич", "колонка A, in.xlsx", Some(Kind::FullName)),
        ("ЛАПТЁВА АННА", "колонка B, in.xlsx", Some(Kind::FullName)),
    ];
    for sheet in ["Семёнов", "СЕМЕНОВ", "семенов", "Лаптева", "ЛАПТЕВА"] {
        let p = profile(vec![Rule::new(sheet, 0, 2, None, Kind::FullName)]);
        let w = privacy_risks(&p, values.iter().copied());
        assert_eq!(w.len(), 1, "{sheet}: {w:?}");
        assert!(w[0].contains("совпадает со значением"), "{sheet}: {w:?}");
    }
}

/// Заголовок-карточка «Менеджер: Иванов Иван Иванович» (сам на ФИО не похож — четыре слова) содержит
/// значение под правилом ФИО целиком — не название колонки. Мутант «0.6 приватность: ФИО внутри заголовка».
#[test]
fn m02_value_inside_card_header_hidden() {
    let d = tmp("m02");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Л");
    sh.extend([(1, 0, s("Менеджер: Иванов Иван Иванович")), (1, 1, s("Склад")), (1, 2, s("Период"))]);
    sh.extend([(3, 0, s("Ответственный")), (3, 1, s("Покупатель"))]);
    sh.extend([(4, 0, s("Иванов Иван Иванович")), (4, 1, s("ООО «Лютик»"))]);
    sh.extend([(5, 0, s("Кузнецов Олег")), (5, 1, s("Кузнецов Олег"))]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Л", 0, 4, None, Kind::FullName), Rule::new("Л", 1, 4, None, Kind::Company)];
    let rep = go(&input, &d.join("out.xlsx"), None, None, rules());
    assert_eq!(rep.rule_heads[0], None);
    let text = journal(&rep, rules());
    assert!(!text.contains("Иванов"), "{text}");
    let pv = preview(&input, &profile(rules()), None).unwrap();
    assert_eq!(pv[0].head, None);
}

/// Подпись шаблона в первом правиле не сдвигает проверку следующих: лист второго правила назван по
/// сотруднику — предупреждение именно у второго правила. Мутант «0.6 профиль: после правила с подписью …».
#[test]
fn m03_risks_attributed_after_label() {
    let mut t = Rule::new("Продажи", 2, 2, None, Kind::Text);
    t.label = "Должность".into();
    let rules = vec![t, Rule::new("Кузнецов", 0, 2, None, Kind::FullName), Rule::new("Итоги", 0, 2, None, Kind::FullName)];
    let values = [("Кузнецов Олег Ильич", "колонка A, in.xlsx", Some(Kind::FullName))];
    let r = rule_risks(&rules, values.iter().copied());
    assert_eq!(r.len(), 3);
    assert!(r[0].is_empty(), "{r:?}");
    assert_eq!(r[1].len(), 1, "{r:?}");
    assert!(r[1][0].contains("имя листа «К***»"), "{r:?}");
    assert!(r[2].is_empty(), "{r:?}");
}

/// Пакет: лист второго файла назван по сотруднику из его данных — предупреждение в отчёте второго файла
/// и на карточке его правила. Мутант «0.6 профиль: листы второго файла пакета не проверяются».
#[test]
fn m04_second_file_sheet_checked() {
    let d = tmp("m04");
    let (a, b) = (d.join("Отчет.xlsx"), d.join("Сотрудники.xlsx"));
    people_book(&a, "Л", &["Петров Пётр"]);
    people_book(&b, "Кузнецов", &["Кузнецов Олег Ильич"]);
    let ra = vec![Rule::new("Л", 0, 2, None, Kind::FullName)];
    let rb = vec![Rule::new("Кузнецов", 0, 2, None, Kind::FullName)];
    let batch = Batch {
        files: vec![
            FileJob { input: a.clone(), output: d.join("o1.xlsx"), rules: ra.clone() },
            FileJob { input: b.clone(), output: d.join("o2.xlsx"), rules: rb.clone() },
        ],
        settings: settings(),
        key_in: None,
        key_out: None,
    };
    let reps = run_batch(&batch, &mut |_| {}).unwrap();
    assert!(!reps[0].warnings.iter().any(|w| w.contains("в профиле")), "{:?}", reps[0].warnings);
    let w: Vec<&String> = reps[1].warnings.iter().filter(|w| w.contains("в профиле")).collect();
    assert_eq!(w.len(), 1, "{:?}", reps[1].warnings);
    assert!(w[0].contains("имя листа «К***»") && !w[0].contains("Кузнецов"), "{w:?}");
    let pv = preview_batch(&[(a, ra), (b, rb)], &settings(), None).unwrap();
    assert_eq!(pv[1].len(), 1);
    assert_eq!(pv[1][0].risks.len(), 1, "{:?}", pv[1][0].risks);
}

/// Лист назван по человеку, который есть только в ключе прошлых запусков: предупреждение и при обработке,
/// и в предпросмотре, и при «Сохранить профиль…». Мутанты «0.6 профиль: имя листа из ключа …» и
/// «0.6 профиль: «Сохранить профиль…» не сверяет листы с ключом».
#[test]
fn m05_sheet_named_like_key_value() {
    let d = tmp("m05");
    let key = d.join("ключ.csv");
    let (a, b) = (d.join("Прошлый.xlsx"), d.join("Новый.xlsx"));
    people_book(&a, "Л", &["Кузнецов Олег Ильич", "Петров Пётр"]);
    go(&a, &d.join("o1.xlsx"), None, Some(&key), vec![Rule::new("Л", 0, 2, None, Kind::FullName)]);
    people_book(&b, "Кузнецов", &["Сидоров Глеб"]);
    let rules = || vec![Rule::new("Кузнецов", 0, 2, None, Kind::FullName)];

    // Без ключа совпадать не с чем.
    let rep = go(&b, &d.join("o2.xlsx"), None, None, rules());
    assert!(!rep.warnings.iter().any(|w| w.contains("в профиле")), "{:?}", rep.warnings);

    let rep = go(&b, &d.join("o3.xlsx"), Some(&key), None, rules());
    let w: Vec<&String> = rep.warnings.iter().filter(|w| w.contains("в профиле")).collect();
    assert_eq!(w.len(), 1, "{:?}", rep.warnings);
    assert!(w[0].contains("ключ прошлых запусков") && w[0].contains("имя листа «К***»"), "{w:?}");
    assert!(!w[0].contains("Кузнецов"), "{w:?}");

    let pv = preview(&b, &profile(rules()), Some(&key)).unwrap();
    assert_eq!(pv[0].risks.len(), 1, "{:?}", pv[0].risks);

    let files: Vec<(PathBuf, Vec<Rule>)> = vec![(b.clone(), rules())];
    assert!(profile_risks(&profile(rules()), &files, None).unwrap().is_empty());
    let r = profile_risks(&profile(rules()), &files, Some(&key)).unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("ключ прошлых запусков") && !r[0].contains("Кузнецов"), "{r:?}");
}

fn rule(kind: Kind) -> Rule {
    let mut r = Rule::new("Л", 0, 2, None, kind);
    r.with_id = false;
    r
}

/// В ячейке ключа несколько адресов через «;»: второй — тоже чей-то настоящий адрес, фейком нового значения
/// он не становится. Мутант «0.6 ключ: фейк email равен второму реальному адресу из ячейки ключа».
#[test]
fn m06_key_email_cell_addresses_all_protected() {
    let r = rule(Kind::Email);
    let orig = "ivan.petrov@corp.ru";
    let e = Faker::new(&settings()).fake(&r, orig, false);
    assert!(e.contains('@') && e != orig, "{e}");
    let mut f = Faker::new(&settings());
    f.load_entries(vec![KeyEntry {
        kind_key: r.kind_key(),
        id: None,
        real: format!("boss@corp.ru; {e}"),
        fake: "a.b@example.com; c.d@example.com".into(),
    }]);
    let e2 = f.fake(&r, orig, false);
    assert_ne!(norm_text(&e2), norm_text(&e), "фейк — настоящий адрес из ключа");
}

/// В ключе — настоящая организация «АО «Y»»; название Y (без формы) — тоже её данные: новая организация
/// фейком «… «Y»» не получает. Мутант «0.6 ключ: фейк организации равен реальному названию из ключа».
#[test]
fn m07_key_company_name_protected() {
    let r = rule(Kind::Company);
    let orig = "ООО «Альфа-Строй»";
    let y = split_company(&Faker::new(&settings()).fake(&r, orig, false)).1;
    assert!(!y.trim().is_empty() && y != "Альфа-Строй", "{y}");
    let mut f = Faker::new(&settings());
    f.load_entries(vec![KeyEntry { kind_key: r.kind_key(), id: None, real: format!("АО «{y}»"), fake: "АО «Совсем Другое»".into() }]);
    let y2 = split_company(&f.fake(&r, orig, false)).1;
    assert_ne!(norm_text(&y2), norm_text(&y), "фейк — название настоящей организации из ключа");
}

/// Те же имена и основы отчеств, что у генератора: настоящие «Имя Отчество» занимают все сочетания словарей.
const MALE: &[&str] = &[
    "Александр", "Алексей", "Анатолий", "Андрей", "Антон", "Аркадий", "Арсений", "Артём", "Борис",
    "Вадим", "Валентин", "Валерий", "Василий", "Виктор", "Виталий", "Владимир", "Владислав",
    "Всеволод", "Вячеслав", "Геннадий", "Георгий", "Глеб", "Григорий", "Даниил", "Денис",
    "Дмитрий", "Евгений", "Егор", "Захар", "Иван", "Игорь", "Илья", "Кирилл", "Константин", "Лев",
    "Леонид", "Максим", "Марк", "Матвей", "Михаил", "Никита", "Николай", "Олег", "Павел", "Пётр",
    "Роман", "Ростислав", "Руслан", "Святослав", "Семён", "Сергей", "Станислав", "Степан",
    "Тимофей", "Тимур", "Филипп", "Фёдор", "Эдуард", "Юрий", "Ярослав",
];

const PATR: &[&str] = &[
    "Александров", "Алексеев", "Анатольев", "Андреев", "Антонов", "Аркадьев", "Борисов", "Вадимов",
    "Валерьев", "Васильев", "Викторов", "Витальев", "Владимиров", "Вячеславов", "Геннадьев",
    "Георгиев", "Григорьев", "Денисов", "Дмитриев", "Евгеньев", "Егоров", "Иванов", "Игорев",
    "Кириллов", "Константинов", "Леонидов", "Максимов", "Михайлов", "Николаев", "Олегов",
    "Павлов", "Петров", "Романов", "Сергеев", "Станиславов", "Степанов", "Тимуров", "Фёдоров",
    "Эдуардов", "Юрьев",
];

/// Все 2400 мужских «Имя Отчество» словарей — настоящие: фейки идут перебором по сетке и с номером тёзки.
/// Тот же человек, записанный ЗАГЛАВНЫМИ, получает тот же фейк, а не следующий свободный вариант.
/// Мутант «0.6 уникальность: тот же человек в другом регистре — другой фейк».
#[test]
fn m08_same_person_other_case_same_fake_after_grid() {
    let reals: Vec<String> = MALE.iter().flat_map(|f| PATR.iter().map(move |p| format!("{f} {p}ич"))).collect();
    let mut f = Faker::new(&settings());
    f.prepare_people(reals.iter().map(String::as_str));
    f.note_reals(reals.iter().map(String::as_str));
    let r = rule(Kind::FullName);
    let fakes: Vec<String> = reals.iter().map(|x| f.fake(&r, x, false)).collect();
    for (x, y) in reals.iter().zip(&fakes) {
        let again = f.fake(&r, &x.to_uppercase(), false);
        assert_eq!(norm_text(&again), norm_text(y), "«{x}» заглавными — другой фейк");
    }
}
