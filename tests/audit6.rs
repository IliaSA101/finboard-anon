//! Аудит 0.6.0: воспроизведение находок. Тесты, которые падают на коде 0.6.0, помечены #[ignore]
//! (запуск: `cargo test --release --test audit6 -- --include-ignored`).

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use common::*;
use finboard_anon::anon::{preview, preview_batch, preview_risks, profile_check, profile_risks, run, Job, Report};
use finboard_anon::fake::{norm_text, Faker, KeyEntry};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

fn settings() -> Settings {
    Settings { salt: "a6".into(), ..Settings::default() }
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

/// Как в privacy6: строка-карточка (три текста — её сочтут шапкой), ниже таблица «Ответственный | Покупатель».
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

/// A6-1 (P1). Карточка над таблицей с человеком, которого нет среди значений под правилами (автор отчёта,
/// руководитель), — в обычной для карточки записи: с подписью через двоеточие, с запятой в конце, email
/// и телефон с подписью. `looks_personal` их не узнаёт, сверять не с чем — ФИО/email/телефон остаются
/// «названием колонки» в журнале (строки отчёта и предупреждение о разных типах).
#[test]
fn a01_card_head_with_prefix_leaks_to_journal() {
    let d = tmp("a01");
    let input = d.join("in.xlsx");
    let mut leaked = Vec::new();
    for (real, piece) in [
        ("Ответственный: Сидоров Семён Семёнович", "Сидоров"),
        ("Сидоров Семён Семёнович,", "Сидоров"),
        ("Руководитель Сидоров С.С.", "Сидоров"),
        ("СИДОРОВ СЕМЁН СЕМЁНОВИЧ (менеджер)", "СИДОРОВ"),
        ("сидоров семён семёнович", "сидоров"),
        ("e-mail: sidorov.boss@corp.ru", "sidorov"),
        ("Тел. +7 916 123 45 67", "916 123 45 67"),
    ] {
        card_book(&input, &[real, "Менеджер", "Отдел продаж"]);
        let rep = go(&input, &d.join("out.xlsx"), None, None, card_rules());
        let text = journal(&rep, card_rules());
        let pv = preview(&input, &profile(card_rules()), None).unwrap();
        if text.contains(piece) || rep.rule_heads[0].is_some() || pv[0].head.is_some() {
            leaked.push(format!("«{real}» → head={:?}", rep.rule_heads[0]));
        }
    }
    assert!(leaked.is_empty(), "в журнал как название колонки попали:\n{}", leaked.join("\n"));
}

/// A6-2 (P2). Предупреждение о листе, названном по сотруднику, маскирует имя листа («К***»), а в том же
/// журнале строки отчёта по правилам и предупреждение «Правило …!… кончается на строке» пишут его как есть.
#[test]
fn a02_sheet_name_masked_in_warning_but_plain_in_summary() {
    let d = tmp("a02");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Кузнецов").extend([
        (1, 0, s("Ответственный")),
        (1, 1, s("Сумма")),
        (2, 0, s("Кузнецов Олег Ильич")),
        (2, 1, Cell::N(5.0)),
        (3, 0, s("Петров Пётр")),
    ]);
    write_xlsx(&input, &b);
    let rules = || vec![Rule::new("Кузнецов", 0, 2, Some(2), Kind::FullName)];
    let rep = go(&input, &d.join("out.xlsx"), None, None, rules());
    let text = journal(&rep, rules());
    assert!(text.contains("имя листа «К***»"), "{text}");
    assert!(!text.contains("Кузнецов"), "имя листа замаскировано в предупреждении, но открыто рядом:\n{text}");
}

/// A6-3 (P2). «Сохранить профиль…» вызывает `profile_risks(...).unwrap_or_default()`: если хоть один открытый
/// файл не читается (перемещён, удалён, ключ испорчен), проверка возвращает ошибку и окно молча сохраняет
/// профиль без предупреждений — даже о листе «Иванов Иван», для которого файлы не нужны вовсе.
#[test]
fn a03_profile_risks_lost_on_unreadable_file() {
    let d = tmp("a03");
    let input = d.join("in.xlsx");
    let mut b = Book::default();
    b.sheet("Иванов Иван").extend([(1, 0, s("ФИО")), (2, 0, s("Петров Пётр"))]);
    write_xlsx(&input, &b);
    let rules = vec![Rule::new("Иванов Иван", 0, 2, None, Kind::FullName)];
    let ok = profile_risks(&profile(rules.clone()), &[(input.clone(), rules.clone())], None).unwrap();
    assert_eq!(ok.len(), 1, "{ok:?}");
    // Второй открытый файл исчез с диска (или ключ не читается): окно получает Err → unwrap_or_default() → [].
    let files: Vec<(PathBuf, Vec<Rule>)> = vec![(input.clone(), rules.clone()), (d.join("пропал.xlsx"), rules.clone())];
    let lost_file = profile_risks(&profile(rules.clone()), &files, None).unwrap_or_default();
    // Ключ, выбранный при открытии, с тех пор переименован или перемещён.
    let lost_key = profile_risks(&profile(rules.clone()), &[(input.clone(), rules.clone())], Some(&d.join("ключ.csv"))).unwrap_or_default();
    assert!(lost_file.len() == 1 && lost_key.len() == 1, "предупреждение о листе потеряно: файл {lost_file:?}, ключ {lost_key:?}");
}

const MALE: &[&str] = &[
    "Александр", "Алексей", "Анатолий", "Андрей", "Антон", "Аркадий", "Арсений", "Артём", "Борис",
    "Вадим", "Валентин", "Валерий", "Василий", "Виктор", "Виталий", "Владимир", "Владислав",
    "Всеволод", "Вячеслав", "Геннадий", "Георгий", "Глеб", "Григорий", "Даниил", "Денис",
    "Дмитрий", "Евгений", "Егор", "Захар", "Иван", "Игорь", "Илья", "Кирилл", "Константин", "Лев",
    "Леонид", "Максим", "Марк", "Матвей", "Михаил", "Никита", "Николай", "Олег", "Павел", "Пётр",
    "Роман", "Ростислав", "Руслан", "Святослав", "Семён", "Сергей", "Станислав", "Степан",
    "Тимофей", "Тимур", "Филипп", "Фёдор", "Эдуард", "Юрий", "Ярослав",
];

fn rule(kind: Kind, with_id: bool, link: &str) -> Rule {
    let mut r = Rule::new("Л", 0, 2, None, kind);
    r.with_id = with_id;
    r.link = link.into();
    r
}

/// A6-4 (P2). Правило «Имя» без ID на кадровой выгрузке (Фамилия | Имя | Отчество отдельными колонками):
/// все 60 мужских имён словаря генератора встречаются среди настоящих — свободного имени нет, и фейк
/// становится своим же настоящим именем («Иван» → «Иван»); остальные — чужими настоящими.
#[test]
#[ignore = "находка A6-4: «Имя» без ID — фейк равен своему настоящему, когда все имена словаря реальны"]
fn a04_first_name_fake_equals_own_when_dictionary_exhausted() {
    let r = rule(Kind::FirstName, false, "");
    let reals: Vec<String> = MALE.iter().map(|s| s.to_string()).chain(["Ринат", "Азат", "Тагир"].iter().map(|s| s.to_string())).collect();
    let mut own = Vec::new();
    let mut shared = 0;
    // Соль у каждого запуска своя: проверяем двадцать.
    for salt in 0..20 {
        let mut f = Faker::new(&Settings { salt: format!("a4-{salt}"), ..Settings::default() });
        f.note_reals(reals.iter().map(String::as_str));
        let mut fakes: HashMap<String, Vec<String>> = HashMap::new();
        for x in &reals {
            let y = f.fake(&r, x, false);
            if norm_text(&y) == norm_text(x) {
                own.push(format!("соль {salt}: «{x}» → «{y}»"));
            }
            fakes.entry(norm_text(&y)).or_default().push(x.clone());
        }
        shared += fakes.values().filter(|v| v.len() > 1).count();
    }
    assert!(own.is_empty(), "фейк равен своему настоящему ({} раз; фейков, общих для разных имён: {shared}):\n{}", own.len(), own.join("\n"));
}

/// A6-5 (P2). «Шаблон»: фейк «Должность 2» достаётся значению «Менеджер», хотя «Должность 2» — настоящее
/// значение той же колонки (оно само получает «Должность 1»). ВПР по результату склеит не те строки,
/// а ячейка «Должность 2» вне разметки (другой лист) останется настоящей и совпадёт с чужим фейком.
#[test]
#[ignore = "находка A6-5: фейк «Шаблона» может совпасть с настоящим значением"]
fn a05_template_fake_equals_other_real() {
    let mut f = Faker::new(&settings());
    let mut r = rule(Kind::Text, false, "");
    r.label = "Должность".into();
    let reals = ["Должность 2", "Менеджер"];
    f.note_reals(reals.iter().copied());
    let real: HashSet<String> = reals.iter().map(|x| norm_text(x)).collect();
    for x in reals {
        let y = f.fake(&r, x, false);
        assert!(!real.contains(&norm_text(&y)), "«{x}» → «{y}» — равно настоящему значению");
    }
}

/// A6-6 (P2). В ячейке несколько адресов или телефонов через «;» (обычное дело в карточке контрагента).
/// `note_real` кладёт в занятые только ячейку целиком, а не каждый адрес/номер: фейк другого значения,
/// выбранный раньше, совпадает с настоящим адресом/телефоном из такой ячейки. Ключ (`load_entries`) адреса
/// по «;» делит — прогон нет. Значение-«ловушка» строится как фейк первого значения у того же генератора.
#[test]
#[ignore = "находка A6-6: настоящий email/телефон из ячейки с несколькими значениями не занят для фейков"]
fn a06_multi_value_cell_reals_not_taken() {
    let mut bad = Vec::new();
    for kind in [Kind::Email, Kind::Phone] {
        let r = rule(kind, false, "");
        let first = if kind == Kind::Email { "petr@corp.ru" } else { "+7 (916) 111-22-33" };
        // Какой фейк получит первое значение, если ничего не мешает.
        let trap = Faker::new(&settings()).fake(&r, first, false);
        let multi = if kind == Kind::Email { format!("ivan@corp.ru; {trap}") } else { format!("+7 (916) 444-55-66; {trap}") };
        let reals = [first.to_string(), multi.clone()];
        let mut f = Faker::new(&settings());
        f.note_reals(reals.iter().map(String::as_str));
        let y = f.fake(&r, first, false);
        let _ = f.fake(&r, &multi, false);
        let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
        let same = if kind == Kind::Email { norm_text(&y) == norm_text(&trap) } else { digits(&y) == digits(&trap) };
        if same {
            bad.push(format!("{kind:?}: «{first}» → «{y}» — это настоящее значение из ячейки «{multi}»"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ---------------------------------------------------------------- проверки без находок (инварианты)

/// ФИО во всех видах (полные, «Фамилия И.О.», «И.О. Фамилия», ЗАГЛАВНЫМИ, с ID и без, в двух связях),
/// второй прогон с ключом первого и новыми людьми: фейк не равен ни одному настоящему значению прогона
/// и ключа, без ID разные люди одной связи не получают одного фейка; тот же ключ — те же фейки.
#[test]
fn a10_people_invariants_with_key() {
    let sur = ["Иванов", "Петров", "Сидоров", "Смирнов", "Козлов", "Попов", "Орлов", "Белов"];
    let fem = ["Иванова", "Петрова", "Сидорова", "Смирнова"];
    let mut all = Vec::new();
    for (i, s) in sur.iter().enumerate() {
        for (j, m) in MALE.iter().enumerate().take(30) {
            let p = ["Иванович", "Петрович", "Олегович", "Юрьевич"][(i + j) % 4];
            all.push(format!("{s} {m} {p}"));
            all.push(format!("{s} {}.{}.", m.chars().next().unwrap(), p.chars().next().unwrap()));
            if j % 3 == 0 {
                all.push(format!("{} {m} {p}", s.to_uppercase()));
                all.push(format!("{}.{}. {s}", m.chars().next().unwrap(), p.chars().next().unwrap()));
            }
        }
    }
    for s in fem {
        for n in ["Анна", "Мария", "Елена", "Ольга"] {
            all.push(format!("{s} {n} Петровна"));
            all.push(format!("{s} {}.П.", n.chars().next().unwrap()));
        }
    }
    let mut seen = HashSet::new();
    all.retain(|x| seen.insert(x.clone()));
    let old: Vec<String> = all.iter().step_by(2).cloned().collect();
    let new: Vec<String> = all.iter().skip(1).step_by(2).cloned().collect();
    let rules = [rule(Kind::FullName, false, ""), rule(Kind::FullName, false, "к"), rule(Kind::FullName, true, "")];
    let feed = |f: &mut Faker, xs: &[String]| -> Vec<(usize, String, String)> {
        f.prepare_people(xs.iter().map(String::as_str));
        f.note_reals(xs.iter().map(String::as_str));
        let mut out = Vec::new();
        for (ri, r) in rules.iter().enumerate() {
            for x in xs {
                out.push((ri, x.clone(), f.fake(r, x, false)));
            }
        }
        out
    };
    let mut f1 = Faker::new(&settings());
    let first = feed(&mut f1, &old);
    let key: Vec<KeyEntry> = f1.entries.clone();
    let reals: HashSet<String> = all.iter().map(|x| norm_text(x)).collect();
    let check = |res: &[(usize, String, String)], prior: &[(usize, String, String)]| {
        let mut owner: HashMap<(usize, String), String> = HashMap::new();
        for (ri, x, y) in prior.iter().chain(res) {
            assert!(!reals.contains(&norm_text(y)), "«{x}» → «{y}» равно настоящему");
            if *ri == 2 {
                continue;
            }
            let o = owner.entry((*ri, norm_text(y))).or_insert_with(|| norm_text(x));
            // «Иванов И.И.» законно делит фейк с полным ФИО, к которому привязан.
            let same = o == &norm_text(x) || o.split(' ').next() == norm_text(x).split(' ').next() && (o.contains('.') || norm_text(x).contains('.'));
            assert!(same, "правило {ri}: «{o}» и «{x}» → один фейк «{y}»");
        }
    };
    check(&first, &[]);
    let second = |k: &[KeyEntry]| {
        let mut f = Faker::new(&settings());
        f.load_entries(k.to_vec());
        feed(&mut f, &new)
    };
    let a = second(&key);
    check(&a, &first);
    assert_eq!(a, second(&key), "тот же ключ — другие фейки");
}

/// A6-7 (P2). `profile_risks` вызывался прямо в потоке окна при «Сохранить профиль…» и читал до 20 000 строк
/// каждого открытого файла: пять файлов по 20 000 строк × 20 колонок — 7–10 секунд (около 2 с на файл),
/// окно «не отвечает». Теперь окно берёт проверку из готового предпросмотра (`preview_risks`, файлы не читаются),
/// а при устаревшем — проверяет в фоне (тесты окна `a6_7_*` в main.rs). Здесь: проверка из предпросмотра
/// мгновенна и даёт то же, что полная проверка по файлам (лист пятого файла назван как значение первого).
#[test]
fn a11_profile_risks_time_in_ui_thread() {
    let d = tmp("a11");
    let mut files: Vec<(PathBuf, Vec<Rule>)> = Vec::new();
    for fi in 0..5 {
        let p = d.join(format!("f{fi}.xlsx"));
        let mut b = Book::default();
        let sheet = if fi == 4 { "Значение 0 2 0" } else { "Л" };
        let sh = b.sheet(sheet);
        for c in 0..20u32 {
            sh.push((1, c, s(&format!("Колонка {c}"))));
        }
        for r in 2..20_002u32 {
            for c in 0..20u32 {
                sh.push((r, c, s(&format!("Значение {fi} {r} {c}"))));
            }
        }
        write_xlsx(&p, &b);
        files.push((p, (0..20).map(|c| Rule::new(sheet, c, 2, None, if c % 2 == 0 { Kind::FullName } else { Kind::Text })).collect()));
    }
    // Предпросмотр считается в фоне при каждой правке правил — к сохранению профиля он уже готов.
    let pv = preview_batch(&files, &settings(), None).unwrap();
    let t = Instant::now();
    let r = preview_risks(&pv[4]);
    let took = t.elapsed();
    eprintln!("проверка профиля из предпросмотра: 5 файлов × 20 000 строк × 20 колонок — {took:?}, предупреждений {}", r.len());
    assert!(took.as_secs_f64() < 0.2, "окно замирает на {took:?}");
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].contains("имя листа «З*** *** *** ***»") && r[0].contains("колонка A, f0.xlsx"), "{r:?}");
    let full = profile_check(&profile(files[4].1.clone()), &files, None);
    assert_eq!(full.risks, r, "из предпросмотра — то же, что по файлам");
    assert!(full.skipped.is_empty());
    assert_eq!(profile_risks(&profile(files[4].1.clone()), &files, None).unwrap(), r);
}
