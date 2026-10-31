//! Связки между колонками и файлами: у человека ФИО, телефон, email, ИНН, СНИЛС, дата рождения, адрес,
//! счёт, код клиента и работодатель с его ИНН. Файлы заказчика содержат разные наборы этих колонок,
//! в разном порядке и в разных форматах. Связка не должна рваться ни в одном месте: одно значение —
//! один фейк в любом файле, любом формате и при любом режиме (пакет или по очереди с ключом).

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::*;
use finboard_anon::anon::{run, run_batch, Batch, FileJob, Job};
use finboard_anon::fake::{company_key, days_from_civil, strip_id_suffix};
use finboard_anon::profile::{Kind, Profile, Rule, Settings};

struct Person {
    fio: &'static str,
    phone: &'static str,
    email: &'static str,
    inn: &'static str,
    snils: &'static str,
    birth: (i64, i64, i64),
    address: &'static str,
    account: &'static str,
    client: &'static str,
    org: usize,
}

const ORGS: [(&str, &str); 3] = [("Ромашка", "7707083893"), ("Лютик", "7728168971"), ("Ясень", "7710140679")];

fn people() -> Vec<Person> {
    vec![
        Person { fio: "Иванов Иван Иванович", phone: "79161234567", email: "ivanov@mail.ru", inn: "500100732259", snils: "11223344595", birth: (1980, 7, 15), address: "г. Москва, ул. Ленина, д. 5, кв. 12", account: "40817810099910004312", client: "К-0012", org: 0 },
        Person { fio: "Петрова Анна Сергеевна", phone: "79035551122", email: "petrova.a@yandex.ru", inn: "770501234567", snils: "11223344696", birth: (1975, 3, 2), address: "г. Тверь, ул. Мира, д. 1", account: "40817810500000123456", client: "К-0044", org: 1 },
        Person { fio: "Сидоров Петр Николаевич", phone: "79267778899", email: "sidorov@gmail.com", inn: "771234567890", snils: "12345678964", birth: (1990, 12, 31), address: "г. Казань, ул. Баумана, д. 20", account: "40817810800000654321", client: "К-0101", org: 0 },
        Person { fio: "Кузнецова Мария Олеговна", phone: "79991112233", email: "kuz.maria@mail.ru", inn: "772987654321", snils: "98765432110", birth: (1985, 1, 1), address: "г. Самара, ул. Победы, д. 7, кв. 3", account: "40817810200000111222", client: "К-0150", org: 2 },
    ]
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Attr {
    Fio,
    Phone,
    Email,
    Inn,
    Snils,
    Birth,
    Address,
    Account,
    Client,
    Org,
    OrgInn,
}

const ATTRS: [Attr; 11] = [Attr::Fio, Attr::Phone, Attr::Email, Attr::Inn, Attr::Snils, Attr::Birth, Attr::Address, Attr::Account, Attr::Client, Attr::Org, Attr::OrgInn];

fn kind(a: Attr) -> Kind {
    match a {
        Attr::Fio => Kind::FullName,
        Attr::Phone => Kind::Phone,
        Attr::Email => Kind::Email,
        Attr::Inn | Attr::OrgInn => Kind::Inn,
        Attr::Snils => Kind::Snils,
        Attr::Birth => Kind::BirthDate,
        Attr::Address => Kind::Address,
        Attr::Account => Kind::Account,
        Attr::Client => Kind::Text,
        Attr::Org => Kind::Company,
    }
}

fn excel_serial(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d) - days_from_civil(1899, 12, 30)
}

/// Значение атрибута в одном из форматов `v`.
fn cell(p: &Person, a: Attr, v: u32) -> Cell {
    let spaced = |d: &str| d.chars().enumerate().flat_map(|(i, c)| if i > 0 && i % 3 == 0 { vec![' ', c] } else { vec![c] }).collect::<String>();
    match a {
        Attr::Fio => s(p.fio),
        Attr::Phone => match v % 4 {
            0 => Cell::N(p.phone.parse().unwrap()),
            1 => s(&format!("+7 ({}) {}-{}-{}", &p.phone[1..4], &p.phone[4..7], &p.phone[7..9], &p.phone[9..])),
            2 => s(&format!("8{}", &p.phone[1..])),
            _ => s(&format!("8 {} {} {} {}", &p.phone[1..4], &p.phone[4..7], &p.phone[7..9], &p.phone[9..])),
        },
        Attr::Email => match v % 2 {
            0 => s(p.email),
            _ => s(&p.email.to_uppercase()),
        },
        Attr::Inn => match v % 3 {
            0 => Cell::N(p.inn.parse().unwrap()),
            1 => s(&spaced(p.inn)),
            _ => s(p.inn),
        },
        Attr::Snils => match v % 2 {
            0 => s(&format!("{}-{}-{} {}", &p.snils[..3], &p.snils[3..6], &p.snils[6..9], &p.snils[9..])),
            _ => s(p.snils),
        },
        Attr::Birth => {
            let (y, m, d) = p.birth;
            match v % 3 {
                0 => Cell::N(excel_serial(y, m, d) as f64),
                1 => s(&format!("{d:02}.{m:02}.{y}")),
                _ => s(&format!("{y}-{m:02}-{d:02}")),
            }
        }
        Attr::Address => match v % 2 {
            0 => s(p.address),
            _ => s(&p.address.replace(", ", ",  ").to_uppercase()),
        },
        Attr::Account => match v % 2 {
            0 => s(p.account),
            _ => s(&format!("{} {} {} {}", &p.account[..5], &p.account[5..8], &p.account[8..9], &p.account[9..])),
        },
        Attr::Client => s(p.client),
        Attr::Org => {
            let o = ORGS[p.org].0;
            s(&match v % 4 {
                0 => format!("ООО «{o}»"),
                1 => format!("ООО \"{o}\""),
                2 => o.to_uppercase(),
                _ => format!("{o}, ООО"),
            })
        }
        Attr::OrgInn => match v % 2 {
            0 => Cell::N(ORGS[p.org].1.parse().unwrap()),
            _ => s(ORGS[p.org].1),
        },
    }
}

/// Нормальная форма фейка — то, что должно совпадать везде, где стоит одно и то же значение.
fn norm(a: Attr, out: &str) -> String {
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    match a {
        Attr::Phone => {
            let d = digits(out);
            d[d.len() - 10..].to_string()
        }
        Attr::Inn | Attr::OrgInn | Attr::Snils | Attr::Account => digits(out),
        Attr::Email => out.to_lowercase(),
        Attr::Birth => {
            if let Ok(v) = out.parse::<f64>() {
                return (v as i64).to_string();
            }
            let p: Vec<i64> = out.split(['.', '-']).map(|x| x.parse().unwrap()).collect();
            let (y, m, d) = if p[0] > 31 { (p[0], p[1], p[2]) } else { (p[2], p[1], p[0]) };
            excel_serial(y, m, d).to_string()
        }
        Attr::Org => company_key(out),
        Attr::Address => strip_id_suffix(out).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase(),
        Attr::Fio | Attr::Client => strip_id_suffix(out).to_string(),
    }
}

fn real_norm(p: &Person, a: Attr) -> String {
    match a {
        Attr::Phone => p.phone[1..].to_string(),
        Attr::Inn => p.inn.into(),
        Attr::OrgInn => ORGS[p.org].1.into(),
        Attr::Snils => p.snils.into(),
        Attr::Account => p.account.into(),
        Attr::Email => p.email.into(),
        Attr::Birth => excel_serial(p.birth.0, p.birth.1, p.birth.2).to_string(),
        Attr::Org => company_key(ORGS[p.org].0),
        Attr::Address => p.address.to_lowercase(),
        Attr::Fio => p.fio.into(),
        Attr::Client => p.client.into(),
    }
}

#[test]
fn every_link_survives_across_files_formats_and_modes() {
    let ps = people();
    let mut seed: u64 = 0x11AC5;
    let mut next = |n: u32| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) % n as u64) as u32
    };
    for round in 0..8 {
        let d = tmp(&format!("links_{round}"));
        // (файл, строка, колонка, атрибут, человек)
        let mut truth: Vec<(usize, u32, u32, Attr, usize, bool)> = Vec::new();
        let mut files: Vec<(PathBuf, Vec<Rule>)> = Vec::new();
        for f in 0..3 {
            // Свой набор колонок в своём порядке.
            let mut attrs: Vec<Attr> = ATTRS.iter().copied().filter(|_| next(3) != 0).collect();
            if attrs.len() < 3 {
                attrs = vec![Attr::Fio, Attr::Phone, Attr::Inn];
            }
            for i in (1..attrs.len()).rev() {
                attrs.swap(i, next(i as u32 + 1) as usize);
            }
            let mut book = Book::default();
            let sh = book.sheet("Данные");
            let mut rules = Vec::new();
            for (c, a) in attrs.iter().enumerate() {
                sh.push((1, c as u32, s(&format!("{a:?}"))));
                let mut r = Rule::new("Данные", c as u32, 2, None, kind(*a));
                if r.kind.id_optional() && next(2) == 0 {
                    r.with_id = !r.with_id;
                }
                if r.kind == Kind::Text {
                    r.label = "Клиент".into();
                }
                rules.push(r);
            }
            let n = 2 + next(5);
            for i in 0..n {
                let row = 2 + i;
                let pi = next(ps.len() as u32) as usize;
                for (c, a) in attrs.iter().enumerate() {
                    let v = cell(&ps[pi], *a, next(4));
                    let numeric = matches!(v, Cell::N(_));
                    sh.push((row, c as u32, v));
                    truth.push((f, row, c as u32, *a, pi, numeric));
                }
            }
            let path = d.join(format!("f{f}.xlsx"));
            write_xlsx(&path, &book);
            files.push((path, rules));
        }
        for mode in ["пакет", "очередь"] {
            let outs: Vec<PathBuf> = (0..3).map(|f| d.join(format!("{mode}_{f}.xlsx"))).collect();
            let key = d.join(format!("{mode}_key.csv"));
            let mut leaks = Vec::new();
            if mode == "пакет" {
                let batch = Batch {
                    files: files.iter().zip(&outs).map(|((i, r), o)| FileJob { input: i.clone(), output: o.clone(), rules: r.clone() }).collect(),
                    settings: Settings { salt: format!("s{round}"), ..Settings::default() },
                    key_in: None,
                    key_out: Some(key.clone()),
                };
                for r in run_batch(&batch, &mut |_| {}).unwrap() {
                    leaks.extend(r.leaks);
                }
            } else {
                for (f, ((i, r), o)) in files.iter().zip(&outs).enumerate() {
                    let job = Job {
                        input: i.clone(),
                        output: o.clone(),
                        key_in: (f > 0).then(|| key.clone()),
                        key_out: Some(key.clone()),
                        profile: Profile { rules: r.clone(), settings: Settings { salt: format!("s{round}-{f}"), ..Settings::default() } },
                    };
                    leaks.extend(run(&job, &mut |_| {}).unwrap().leaks);
                }
            }
            let grids: Vec<_> = outs.iter().map(|o| read_book(o)).collect();
            let mut seen: HashMap<(Attr, usize), (String, String)> = HashMap::new();
            for (f, row, col, a, pi, numeric) in &truth {
                let got = grids[*f]["Данные"].get(*row, *col);
                let out = got.map(|c| c.text.clone()).unwrap_or_default();
                // Число остаётся числом: иначе ВПР и суммы по этой колонке ломаются.
                assert_eq!(got.is_some_and(|c| c.numeric), *numeric, "{mode}, раунд {round}: {a:?} «{out}» сменил тип");
                let n = norm(*a, &out);
                assert_ne!(n, real_norm(&ps[*pi], *a), "{mode}, раунд {round}: {a:?} не заменён: {out}");
                let (prev, prev_out) = seen.entry((*a, *pi)).or_insert_with(|| (n.clone(), out.clone()));
                assert_eq!(prev, &n, "{mode}, раунд {round}, {a:?} у {}: файл {f}: «{out}», раньше «{prev_out}»", ps[*pi].fio);
            }
            assert!(leaks.is_empty(), "{mode}, раунд {round}: {leaks:?}");
        }
    }
}
