//! «Связь» правила (0.5.0): колонки с разной связью получают для одного реального значения разные фейки,
//! внутри одной связи — один фейк во всех файлах и листах. Пустая связь — ровно прежнее поведение.
//! Проверки — по всем типам, которые дают фейки, а не по одной паре колонок.

mod common;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use common::*;
use finboard_anon::anon::{run_batch, Batch, FileJob, Report};
use finboard_anon::profile::{link_name, parse_kind_key, split_link, Kind, Profile, Rule, Settings};

/// Типы, которые дают фейки (у «Очистить» и «Не менять» фейков нет), и по три реальных значения каждого.
fn cases() -> Vec<(Kind, [&'static str; 3])> {
    vec![
        (Kind::FullName, ["Иванов Иван Иванович", "Петрова Анна Сергеевна", "Сидоров Пётр Николаевич"]),
        (Kind::LastName, ["Кузнецов", "Смирнова", "Волков"]),
        (Kind::FirstName, ["Михаил", "Ольга", "Дмитрий"]),
        (Kind::MiddleName, ["Андреевич", "Викторовна", "Олегович"]),
        (Kind::Email, ["ivanov@mail.ru", "petrova.a@yandex.ru", "sidorov@gmail.com"]),
        (Kind::Phone, ["+7 (916) 123-45-67", "+7 (903) 555-11-22", "+7 (926) 777-88-99"]),
        (Kind::Inn, ["500100732259", "7707083893", "771234567890"]),
        (Kind::Snils, ["112-233-445 95", "112-233-446 96", "123-456-789 64"]),
        (Kind::Passport, ["45 06 123456", "46 11 654321", "40 03 111222"]),
        (Kind::Address, ["г. Москва, ул. Ленина, д. 5", "г. Тверь, ул. Мира, д. 1", "г. Казань, ул. Баумана, д. 20"]),
        (Kind::BirthDate, ["15.07.1980", "02.03.1975", "31.12.1990"]),
        (Kind::Company, ["ООО «Ромашка»", "АО «Лютик»", "ООО «Ясень»"]),
        (Kind::Account, ["40817810099910004312", "40817810500000123456", "40817810800000654321"]),
        (Kind::Mask, ["АБ-123456-ВГ", "ДЕ-654321-ЖЗ", "КЛ-111222-МН"]),
        (Kind::Text, ["Главный бухгалтер", "Старший кассир", "Финансовый директор"]),
    ]
}

/// Колонка типа `ki` в связи: чётная — общая связь, нечётная — связь `link`.
fn col(ki: usize, in_link: bool) -> u32 {
    (ki * 2 + in_link as usize) as u32
}

/// Правила: у каждого типа две колонки с одинаковыми значениями — общая и со связью `link`.
fn rules(link: &str, flip_id: bool) -> Vec<Rule> {
    let mut out = Vec::new();
    for (ki, (kind, _)) in cases().into_iter().enumerate() {
        for in_link in [false, true] {
            let mut r = Rule::new("Данные", col(ki, in_link), 2, None, kind);
            if flip_id && kind.id_optional() {
                r.with_id = !r.with_id;
            }
            if kind == Kind::Text {
                r.label = "Должность".into();
            }
            if in_link {
                r.link = link.into();
            }
            out.push(r);
        }
    }
    out
}

/// Книга: шапка и три строки; `reverse` — строки в обратном порядке (второй файл пакета).
fn book(path: &Path, reverse: bool) {
    let mut b = Book::default();
    let sh = b.sheet("Данные");
    for (ki, (kind, vals)) in cases().into_iter().enumerate() {
        for in_link in [false, true] {
            let c = col(ki, in_link);
            sh.push((1, c, s(&format!("{} {}", kind.title(), if in_link { "контрагента" } else { "организации" }))));
            for (vi, v) in vals.iter().enumerate() {
                let row = if reverse { 4 - vi as u32 } else { 2 + vi as u32 };
                sh.push((row, c, s(v)));
            }
        }
    }
    write_xlsx(path, &b);
}

fn settings(salt: &str) -> Settings {
    Settings { salt: salt.into(), ..Settings::default() }
}

fn batch(files: &[(PathBuf, PathBuf, Vec<Rule>)], salt: &str, key_in: Option<&Path>, key_out: Option<&Path>) -> Vec<Report> {
    let b = Batch {
        files: files.iter().map(|(i, o, r)| FileJob { input: i.clone(), output: o.clone(), rules: r.clone() }).collect(),
        settings: settings(salt),
        key_in: key_in.map(Path::to_path_buf),
        key_out: key_out.map(Path::to_path_buf),
    };
    run_batch(&b, &mut |_| {}).unwrap()
}

/// Фейк значения `vi` типа `ki` в результате: строка зависит от того, перевёрнут ли файл.
fn fake_at(out: &Path, ki: usize, vi: usize, in_link: bool, reverse: bool) -> String {
    let g = read_book(out);
    let row = if reverse { 4 - vi as u32 } else { 2 + vi as u32 };
    g["Данные"].get(row, col(ki, in_link)).map(|c| c.text.clone()).unwrap_or_default()
}

/// Части книги без сжатия: имя → байты (сравнение результатов без оглядки на упаковку zip).
fn parts(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut v = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        v.push((f.name().to_string(), data));
    }
    v
}

/// Для каждого типа: одно значение в общей колонке и в колонке со связью — разные фейки; в каждой связи —
/// один фейк в обоих файлах пакета; у типов с ID фейки всех значений обеих связей разные (общий счётчик ID).
/// Предупреждения о «разных типах» нет — разделение задумано; о разных фейках вне разметки — одна строка.
#[test]
fn every_kind_separated_by_link_and_consistent_inside() {
    for flip_id in [false, true] {
        let d = tmp(&format!("link_kinds_{flip_id}"));
        let (a, b) = (d.join("a.xlsx"), d.join("b.xlsx"));
        book(&a, false);
        book(&b, true);
        let rs = rules("Контрагент", flip_id);
        let files = vec![(a.clone(), d.join("a_out.xlsx"), rs.clone()), (b.clone(), d.join("b_out.xlsx"), rs.clone())];
        let reps = batch(&files, "соль-связи", None, Some(&d.join("key.csv")));
        for (fi, r) in reps.iter().enumerate() {
            assert!(r.leaks.is_empty(), "файл {fi}: {:?}", r.leaks);
            assert!(!r.warnings.iter().any(|w| w.contains("разных типов")), "файл {fi}: {:?}", r.warnings);
            let n = r.warnings.iter().filter(|w| w.contains("разной «Связью»")).count();
            assert_eq!(n, 1, "файл {fi}: {:?}", r.warnings);
            // 15 типов по 3 значения: каждое — в двух связях.
            assert!(r.warnings.iter().any(|w| w.starts_with("45 значений размечены")), "файл {fi}: {:?}", r.warnings);
        }
        let (oa, ob) = (&files[0].1, &files[1].1);
        for (ki, (kind, vals)) in cases().into_iter().enumerate() {
            let with_id = rs[col(ki, false) as usize].with_id_effective();
            let mut all = HashSet::new();
            for (vi, real) in vals.iter().enumerate() {
                let common = fake_at(oa, ki, vi, false, false);
                let linked = fake_at(oa, ki, vi, true, false);
                let ctx = format!("{kind:?} «{real}», с ID: {with_id}");
                assert!(!common.is_empty() && !linked.is_empty(), "{ctx}: пусто");
                assert_ne!(common, *real, "{ctx}: общая колонка не заменена");
                assert_ne!(linked, *real, "{ctx}: колонка со связью не заменена");
                assert_ne!(common, linked, "{ctx}: одинаковый фейк в разных связях");
                assert_eq!(fake_at(ob, ki, vi, false, true), common, "{ctx}: общая связь, второй файл");
                assert_eq!(fake_at(ob, ki, vi, true, true), linked, "{ctx}: связь, второй файл");
                all.insert(common);
                all.insert(linked);
            }
            if with_id {
                assert_eq!(all.len(), 6, "{kind:?}: у разных значений в разных связях совпали фейки: {all:?}");
            }
        }
        // В ключе связь — в колонке «Тип» после «@», строчными; общая — без «@».
        let key = std::fs::read_to_string(d.join("key.csv")).unwrap();
        assert!(key.lines().any(|l| l.starts_with("ФИО+ID@контрагент;") || l.starts_with("ФИО@контрагент;")), "{key}");
        assert!(key.lines().any(|l| l.starts_with("ФИО+ID;") || l.starts_with("ФИО;")), "{key}");
    }
}

/// Профиль без поля связи (до 0.5.0) читается, связь пустая, и результат тот же до байта, что у правил,
/// собранных с пустой связью. Пустая связь в профиль не пишется — профиль читается и прежними версиями.
#[test]
fn old_profile_without_link_gives_same_output() {
    let d = tmp("link_old_profile");
    let a = d.join("a.xlsx");
    book(&a, false);
    let rs = rules("", false);
    let json = serde_json::to_string_pretty(&Profile { rules: rs.clone(), settings: settings("x") }).unwrap();
    assert!(!json.contains("\"link\""), "пустая связь записана в профиль");
    // Профиль 0.4.0: тех же правил, без поля.
    let old: Profile = serde_json::from_str(&json).unwrap();
    assert!(old.rules.iter().all(|r| r.link.is_empty()));
    assert_eq!(old.rules, rs);
    let out1 = d.join("old.xlsx");
    let out2 = d.join("new.xlsx");
    batch(&[(a.clone(), out1.clone(), old.rules.clone())], "та же соль", None, None);
    batch(&[(a.clone(), out2.clone(), rs.clone())], "та же соль", None, None);
    assert!(parts(&out1) == parts(&out2), "профиль без связи дал другой результат");
    // Связь со значением — пишется и читается обратно.
    let mut linked = rs.clone();
    linked[1].link = "Контрагент".into();
    let json = serde_json::to_string(&Profile { rules: linked.clone(), settings: settings("x") }).unwrap();
    assert!(json.contains("\"link\":\"Контрагент\""), "{json}");
    let back: Profile = serde_json::from_str(&json).unwrap();
    assert_eq!(back.rules, linked);
}

/// Ключ со связями: второй прогон с другой солью по ключу первого даёт те же фейки в обеих связях.
/// Ключ 0.4.0 (без «@») восстанавливает только общую связь; у колонки со связью — свой, новый фейк.
#[test]
fn key_round_trip_with_links_and_old_key() {
    let d = tmp("link_key");
    let a = d.join("a.xlsx");
    book(&a, false);
    let rs = rules("контрагент", false);
    let key = d.join("key.csv");
    let o1 = d.join("first.xlsx");
    batch(&[(a.clone(), o1.clone(), rs.clone())], "первая", None, Some(&key));
    let o2 = d.join("second.xlsx");
    let key2 = d.join("key2.csv");
    batch(&[(a.clone(), o2.clone(), rs.clone())], "вторая", Some(&key), Some(&key2));
    for (ki, (kind, vals)) in cases().into_iter().enumerate() {
        for vi in 0..vals.len() {
            for in_link in [false, true] {
                assert_eq!(
                    fake_at(&o1, ki, vi, in_link, false),
                    fake_at(&o2, ki, vi, in_link, false),
                    "{kind:?} «{}», связь: {in_link}",
                    vals[vi]
                );
            }
        }
    }

    // Ключ прошлой версии: только общая связь, без «@».
    let old_key = d.join("old_key.csv");
    let text = std::fs::read_to_string(&key).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.split(';').next().unwrap_or("").contains('@')).collect();
    std::fs::write(&old_key, kept.join("\n")).unwrap();
    let o3 = d.join("third.xlsx");
    batch(&[(a.clone(), o3.clone(), rs.clone())], "третья", Some(&old_key), None);
    for (ki, (kind, vals)) in cases().into_iter().enumerate() {
        for vi in 0..vals.len() {
            let common = fake_at(&o3, ki, vi, false, false);
            assert_eq!(common, fake_at(&o1, ki, vi, false, false), "{kind:?} «{}»: общая связь из старого ключа", vals[vi]);
            assert_ne!(fake_at(&o3, ki, vi, true, false), common, "{kind:?} «{}»: старый ключ попал в связь", vals[vi]);
        }
    }
}

/// Значение, размеченное в нескольких связях, вне разметки получает фейк общей связи, а без общей —
/// связи первого по порядку правила. Каждый прогон (порядок в словарях случайный) — тот же выбор.
#[test]
fn outside_rules_pick_is_deterministic() {
    let d = tmp("link_outside");
    let a = d.join("a.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Данные");
    for (c, h) in ["Покупатель", "Продавец", "Директор", "Банк", "ИНН банка", "ИНН продавца", "Примечание"].iter().enumerate() {
        sh.push((1, c as u32, s(h)));
    }
    // A, B: одно ФИО только в связях «б» и «а» (правила в этом порядке); C, D: другое — в связи «а» и в общей.
    sh.push((2, 0, s("Иванов Иван Иванович")));
    sh.push((2, 1, s("Иванов Иван Иванович")));
    sh.push((2, 2, s("Петров Пётр Петрович")));
    sh.push((2, 3, s("Петров Пётр Петрович")));
    // E, F: ИНН — в общей и в связи «а».
    sh.push((2, 4, s("7707083893")));
    sh.push((2, 5, s("7707083893")));
    // G — вне разметки: те же значения целиком и внутри текста.
    sh.push((2, 6, s("Иванов Иван Иванович")));
    sh.push((3, 6, s("Петров Пётр Петрович")));
    sh.push((4, 6, s("Оплата по ИНН 7707083893 за март")));
    write_xlsx(&a, &b);
    let mut rs = vec![
        Rule::new("Данные", 0, 2, None, Kind::FullName),
        Rule::new("Данные", 1, 2, None, Kind::FullName),
        Rule::new("Данные", 2, 2, None, Kind::FullName),
        Rule::new("Данные", 3, 2, None, Kind::FullName),
        Rule::new("Данные", 4, 2, None, Kind::Inn),
        Rule::new("Данные", 5, 2, None, Kind::Inn),
    ];
    rs[0].link = "б".into();
    rs[1].link = "а".into();
    rs[2].link = "а".into();
    rs[5].link = "А".into();
    for round in 0..4 {
        let out = d.join(format!("out{round}.xlsx"));
        let rep = batch(&[(a.clone(), out.clone(), rs.clone())], &format!("соль {round}"), None, None).remove(0);
        let g = read_book(&out);
        let t = |a1: &str| text(&g, "Данные", a1);
        assert_ne!(t("A2"), t("B2"), "раунд {round}");
        assert_eq!(t("G2"), t("A2"), "раунд {round}: вне разметки — фейк связи первого правила («б»)");
        assert_ne!(t("C2"), t("D2"), "раунд {round}");
        assert_eq!(t("G3"), t("D2"), "раунд {round}: вне разметки — фейк общей связи");
        let digits = |s: String| s.chars().filter(char::is_ascii_digit).collect::<String>();
        assert_ne!(digits(t("E2")), digits(t("F2")), "раунд {round}");
        assert_eq!(digits(t("G4")), digits(t("E2")), "раунд {round}: ИНН в тексте — фейк общей связи");
        assert!(rep.leaks.is_empty(), "раунд {round}: {:?}", rep.leaks);
        let w: Vec<&String> = rep.warnings.iter().filter(|w| w.contains("разной «Связью»")).collect();
        assert_eq!(w.len(), 1, "{:?}", rep.warnings);
        assert!(w[0].starts_with("3 значения размечены"), "{}", w[0]);
        // Строка отчёта у правила со связью — с её именем; у общей — как раньше.
        let lines = rep.summary(&Profile { rules: rs.clone(), settings: settings("") });
        assert!(lines.iter().any(|l| l.contains("правило «ФИО», связь «б»: заменено ячеек 1")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("правило «ФИО»: заменено ячеек 1")), "{lines:?}");
    }
}

/// Предупреждение о разных типах — только внутри одной связи: одно значение под «ФИО» и «Шаблоном»
/// в связи «а» — предупреждение есть; то же значение в общей связи и в «а» под одним типом — нет.
#[test]
fn kind_conflicts_only_inside_one_link() {
    let d = tmp("link_conflicts");
    let a = d.join("a.xlsx");
    let mut b = Book::default();
    let sh = b.sheet("Данные");
    for c in 0..3 {
        sh.push((1, c, s(&format!("Колонка {c}"))));
        sh.push((2, c, s("Иванов Иван Иванович")));
    }
    write_xlsx(&a, &b);
    let mut rs = vec![
        Rule::new("Данные", 0, 2, None, Kind::FullName),
        Rule::new("Данные", 1, 2, None, Kind::FullName),
        Rule::new("Данные", 2, 2, None, Kind::Text),
    ];
    rs[1].link = "а".into();
    let rep = batch(&[(a.clone(), d.join("o1.xlsx"), rs[..2].to_vec())], "с", None, None).remove(0);
    assert!(!rep.warnings.iter().any(|w| w.contains("разных типов")), "{:?}", rep.warnings);
    rs[2].link = "а".into();
    let rep = batch(&[(a.clone(), d.join("o2.xlsx"), rs.clone())], "с", None, None).remove(0);
    let w: Vec<&String> = rep.warnings.iter().filter(|w| w.contains("разных типов")).collect();
    assert_eq!(w.len(), 1, "{:?}", rep.warnings);
    // В предупреждении — только колонки связи «а», общая колонка A в нём не названа.
    assert!(w[0].contains("правило «ФИО»") && w[0].contains("правило «Шаблон"), "{}", w[0]);
    assert!(!w[0].contains("Колонка 0"), "{}", w[0]);
}

/// Имя связи и ключ типа: регистр и пробелы не различают связи, «@» внутри подписи шаблона — не связь.
#[test]
fn link_names_and_kind_keys() {
    assert_eq!(link_name("  Контрагент  "), "контрагент");
    assert_eq!(link_name("Город   контрагента"), "город контрагента");
    assert_eq!(link_name("a@b[c]"), "abc");
    assert_eq!(link_name("   "), "");
    let mut r = Rule::new("Л", 0, 2, None, Kind::Text);
    r.label = "a@b".into();
    assert_eq!(r.kind_key(), "Текст + ID[a@b]");
    assert_eq!(split_link(&r.kind_key()), ("Текст + ID[a@b]", ""));
    r.link = "Контрагент".into();
    assert_eq!(r.kind_key(), "Текст + ID[a@b]@контрагент");
    assert_eq!(split_link(&r.kind_key()), ("Текст + ID[a@b]", "контрагент"));
    assert_eq!(parse_kind_key(&r.kind_key()), Some((Kind::Text, true, "a@b".to_string())));
    let mut p = Rule::new("Л", 0, 2, None, Kind::FullName);
    assert_eq!(p.kind_key(), "ФИО+ID");
    p.link = "Контрагент ".into();
    assert_eq!(p.kind_key(), "ФИО+ID@контрагент");
    assert_eq!(parse_kind_key("ФИО+ID@контрагент"), Some((Kind::FullName, true, String::new())));
    assert_eq!(split_link("Email"), ("Email", ""));
    // Одинаковые по смыслу связи — одна: «Контрагент» и «контрагент» дают один ключ.
    let keys: HashMap<String, ()> = ["Контрагент", "контрагент", " КОНТРАГЕНТ"]
        .iter()
        .map(|l| {
            let mut x = Rule::new("Л", 0, 2, None, Kind::Inn);
            x.link = l.to_string();
            (x.kind_key(), ())
        })
        .collect();
    assert_eq!(keys.len(), 1);
}
