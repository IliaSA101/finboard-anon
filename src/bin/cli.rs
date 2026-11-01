//! Консольный запуск по готовому профилю:
//!
//! finboard-anon-cli --profile profile.json --in data.xlsx --out data_anon.xlsx
//!                   [--key ключ.csv] [--load-key старый_ключ.csv] [--no-key]
//!
//! --no-key — без ключа совсем: найденный ключ не загружается и не трогается, новый не пишется
//! (как «Не использовать» в окне, но и без записи нового ключа).
//!
//! Несколько файлов одного заказчика — одним пакетом, с общими фейками и одним ключом:
//! finboard-anon-cli --profile a.json --in a.xlsx --out a_anon.xlsx --profile b.json --in b.xlsx --out b_anon.xlsx

use std::path::PathBuf;
use std::process::ExitCode;

use finboard_anon::anon::{find_key, key_out_path, run_batch, shared_key_path, Batch, FileJob};
use finboard_anon::profile::Profile;

fn usage() -> ExitCode {
    eprintln!(
        "Использование:\n  finboard-anon-cli --profile profile.json --in data.xlsx --out data_anon.xlsx \
         [--key ключ.csv] [--load-key старый_ключ.csv] [--no-key]
  несколько файлов: повторить --in/--out (и --profile перед файлом, если у него свой профиль)
  ключ по умолчанию — общий для папки первого файла; если он уже есть, он дополняется
  --no-key — без ключа: найденный ключ не загружается и не меняется, новый не записывается
             (фейки и ID — заново; расшифровать такие файлы потом будет нечем)
  finboard-anon-cli --detect data.xlsx   (показать, что найдёт автоподбор)"
    );
    ExitCode::from(2)
}

/// Показывает, какие правила предложит автоподбор.
fn detect(path: &std::path::Path) -> ExitCode {
    use finboard_anon::xlsx::{load_grid, Workbook};
    let wb = match Workbook::open(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Ошибка: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut profile = Profile::default();
    for (i, s) in wb.sheets.iter().enumerate() {
        match load_grid(path, &wb, i, 1_048_576) {
            Ok(g) => profile.rules.extend(finboard_anon::detect::suggest(&s.name, &g)),
            Err(e) => eprintln!("Лист «{}»: {e:#}", s.name),
        }
    }
    for r in &profile.rules {
        println!("{}!{}  {}", r.sheet, r.range_text(), r.kind.title());
    }
    if profile.rules.is_empty() {
        println!("Ничего не найдено");
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.len() == 2 && all[0] == "--detect" {
        return detect(&PathBuf::from(&all[1]));
    }
    let mut args = all.into_iter();
    // Каждый --in начинает новый файл пакета; --profile действует на следующие файлы.
    let (mut profile, mut files, mut key, mut load_key, mut no_key) = (None::<PathBuf>, Vec::new(), None, None, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--profile" => {
                profile = args.next().map(PathBuf::from);
                // Файлы, указанные до первого --profile, обрабатываются по нему.
                for f in files.iter_mut().filter(|f: &&mut (PathBuf, Option<PathBuf>, Option<PathBuf>)| f.2.is_none()) {
                    f.2 = profile.clone();
                }
            }
            "--in" => match args.next() {
                Some(i) => files.push((PathBuf::from(i), None::<PathBuf>, profile.clone())),
                None => return usage(),
            },
            "--out" => match (args.next(), files.last_mut()) {
                (Some(o), Some(f)) if f.1.is_none() => f.1 = Some(PathBuf::from(o)),
                _ => return usage(),
            },
            "--key" => key = args.next().map(PathBuf::from),
            "--load-key" => load_key = args.next().map(PathBuf::from),
            "--no-key" => no_key = true,
            _ => return usage(),
        }
    }
    if files.is_empty() || files.iter().any(|f| f.1.is_none() || f.2.is_none()) {
        return usage();
    }
    let mut jobs = Vec::new();
    let mut settings = None;
    for (input, output, p) in files {
        let pr = match Profile::load(&p.unwrap()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Ошибка: {e:#}");
                return ExitCode::FAILURE;
            }
        };
        settings.get_or_insert(pr.settings);
        jobs.push((FileJob { input, output: output.unwrap(), rules: pr.rules.clone() }, pr.rules));
    }
    // --no-key — без ключа на входе и на выходе. Раньше найденный ключ всё равно загружался: фейки шли из него,
    // хотя просили «без ключа».
    if no_key && (key.is_some() || load_key.is_some()) {
        eprintln!("Ошибка: --no-key нельзя вместе с --key или --load-key");
        return usage();
    }
    // Ключ уже есть (общий ключ папки или старый ключ файла) — подхватываем и дополняем,
    // а не перезаписываем: в нём фейки уже переданных файлов.
    let first = jobs[0].0.input.clone();
    let found = if no_key { None } else { find_key(&first) };
    let key_out = if no_key {
        None
    } else {
        Some(key.unwrap_or_else(|| if load_key.is_some() { shared_key_path(&first) } else { key_out_path(&first, found.as_deref(), false) }))
    };
    let key_in = load_key.or(found);
    let batch = Batch {
        files: jobs.iter().map(|(f, _)| f.clone()).collect(),
        settings: settings.unwrap(),
        key_in,
        key_out,
    };
    match run_batch(&batch, &mut |m| eprintln!("{m}")) {
        Ok(reports) => {
            let mut leaks = false;
            for (r, (_, rules)) in reports.iter().zip(&jobs) {
                for line in r.summary(&Profile { rules: rules.clone(), settings: batch.settings.clone() }) {
                    println!("{line}");
                }
                leaks |= !r.leaks.is_empty();
            }
            if leaks { ExitCode::from(3) } else { ExitCode::SUCCESS }
        }
        Err(e) => {
            eprintln!("Ошибка: {e:#}");
            ExitCode::FAILURE
        }
    }
}
