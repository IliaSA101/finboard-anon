#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke};

use finboard_anon::anon::{
    batch_outputs, default_output_path, find_key, key_out_path, preview_batch, run_batch, Batch, FileJob, Report,
    RulePreview,
};
use finboard_anon::detect;
use finboard_anon::profile::{link_name, Kind, Outside, Profile, Rule, Settings};
use finboard_anon::xlsx::{col_name, load_grid, parse_range, Grid, GridCell, Progress, Workbook};

/// Сколько строк листа загружать для просмотра — столько же читает и предпросмотр правил.
const GRID_ROWS: u32 = finboard_anon::anon::PREVIEW_ROWS;
const GRID_COLS: u32 = 200;
const ROW_H: f32 = 20.0;
const HEAD_H: f32 = 28.0;
const ROWNUM_W: f32 = 52.0;

/// Профиль рядом с исходным файлом: «<имя>_профиль.json».
fn side_profile_path(input: &std::path::Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("книга");
    input.with_file_name(format!("{stem}_профиль.json"))
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn is_excel(p: &std::path::Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("xlsx") || e.eq_ignore_ascii_case("xlsm"))
}

/// Имя без расширения в нижнем регистре.
fn stem_lower(p: &std::path::Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Исходник результата прошлого запуска: «отчёт_anon», «отчёт_anon_2» → «отчёт».
fn anon_source_stem(stem: &str) -> Option<&str> {
    let (base, rest) = stem.rsplit_once("_anon")?;
    (rest.is_empty() || rest.strip_prefix('_').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))).then_some(base)
}

/// Файлы Excel в папке и её подпапках — по порядку имён. Без временных файлов открытой книги («~$…») и без
/// результатов прошлых запусков: «отчёт_anon.xlsx» пропускается, только если рядом лежит его исходник «отчёт.xlsx»
/// (файл, обезличенный заранее другим способом, — тоже исходник).
/// Символические ссылки и точки соединения (junction) не открываются: ссылка на папку выше по дереву
/// зациклила бы обход, а ссылка в чужую папку подмешала бы чужие файлы. Папки OneDrive «по запросу» — тоже
/// точки повторной обработки, но не ссылки (`is_symlink` их не считает ссылками) — они обходятся как обычно.
fn excel_files_in(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut here = Vec::new();
        for e in rd.flatten() {
            // Тип записи каталога — без перехода по ссылке.
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if is_excel(&p) && !stem_lower(&p).starts_with("~$") {
                here.push(p);
            }
        }
        let stems: std::collections::HashSet<String> = here.iter().map(|p| stem_lower(p)).collect();
        out.extend(here.into_iter().filter(|p| !anon_source_stem(&stem_lower(p)).is_some_and(|b| stems.contains(b))));
    }
    out.sort();
    out
}

/// Текст ячейки для просмотра: числа без хвостов вроде 39.200000000000003.
fn cell_display(c: &GridCell) -> String {
    if let Some(d) = &c.date {
        return d.clone();
    }
    if c.numeric {
        if let Ok(v) = c.text.parse::<f64>() {
            return format!("{v}");
        }
    }
    c.text.clone()
}

/// Ширины колонок по содержимому первых строк.
fn column_widths(g: &Grid) -> Vec<f32> {
    let ncols = (g.max_col + 1).min(GRID_COLS);
    (0..ncols)
        .map(|c| {
            let longest = (1..=g.max_row.min(300))
                .filter_map(|r| g.get(r, c))
                .map(|x| cell_display(x).chars().count())
                .max()
                .unwrap_or(0);
            (longest.min(36) as f32 * 7.2 + 14.0).clamp(48.0, 270.0)
        })
        .collect()
}

/// Время по-русски: «45 с», «3 мин 05 с», «1 ч 02 мин» — до целых секунд.
fn format_duration(d: Duration) -> String {
    let s = d.as_secs_f64().round() as u64;
    match s {
        0..=59 => format!("{s} с"),
        60..=3599 => format!("{} мин {:02} с", s / 60, s % 60),
        _ => format!("{} ч {:02} мин", s / 3600, s % 3600 / 60),
    }
}

/// Этап обработки по строке журнала — коротко, для полосы хода; `None` — строка не про этап.
fn stage_of(line: &str) -> Option<&'static str> {
    [("Сбор значений", "сбор значений"), ("Запись файла", "запись"), ("Проверка результата", "проверка результата")]
        .into_iter()
        .find(|(p, _)| line.starts_with(p))
        .map(|(_, s)| s)
}

/// Подсказка к полю «Связь:» на карточке правила.
const LINK_HINT: &str = "Оставьте пустым — одно значение везде получает один фейк. Впишите слово (например, «контрагент») — у колонок с этим словом свои фейки, не связанные с остальными.";

/// Надпись на полосе хода: «37 % — Файл 2 из 10: Продажи.xlsx — запись · осталось ≈ 1 мин 20 с».
/// Остаток — по доле и прошедшему времени, пока доля мала (до 3 %) — не оценивается.
fn progress_label(fraction: f32, stage: &str, file_line: &str, elapsed: Duration) -> String {
    let f = fraction.clamp(0.0, 1.0);
    let mut parts = vec![format!("{} %", (f * 100.0).floor() as u32)];
    parts.extend([file_line, stage].into_iter().filter(|s| !s.is_empty()).map(String::from));
    let tail = if f >= 1.0 {
        format!("за {}", format_duration(elapsed))
    } else if f > 0.03 {
        let left = elapsed.as_secs_f64() * f64::from(1.0 - f) / f64::from(f);
        format!("осталось ≈ {}", format_duration(Duration::from_secs_f64(left)))
    } else {
        "осталось: оценка…".into()
    };
    format!("{} · {tail}", parts.join(" — "))
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Cell(u32, u32),
    Column(u32),
    None,
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1500.0, 920.0])
            .with_min_inner_size([1000.0, 600.0])
            .with_title(format!("Обезличивание Excel {}", env!("CARGO_PKG_VERSION")))
            .with_icon(egui::IconData { rgba: include_bytes!("../assets/icon_256.rgba").to_vec(), width: 256, height: 256 })
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Обезличивание Excel", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

enum Msg {
    Log(String),
    Done(Result<Vec<Report>, String>, Vec<Profile>),
}

/// Прочитанный файл: книга, ошибка чтения или `None` — файл уже открыт (не читался повторно).
type Loaded = (PathBuf, Option<Result<Workbook, String>>);

/// Сообщения фонового открытия файлов.
enum OpenMsg {
    /// Строка хода: «Открытие файлов: 3 из 120 — Продажи.xlsx».
    Progress(String),
    /// Готово: прочитанные файлы, «закрыть прежние», папки без файлов Excel.
    Done(Vec<Loaded>, bool, Vec<PathBuf>),
}

/// Читает книги — тяжёлая часть открытия (общие строки каждого файла). Без окна: годится для фонового потока.
/// Уже открытые (`known`) и повторы в списке не читаются.
fn load_books(paths: Vec<PathBuf>, known: &HashSet<PathBuf>, progress: &mut dyn FnMut(usize, usize, &std::path::Path)) -> Vec<Loaded> {
    let n = paths.len();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (k, path) in paths.into_iter().enumerate() {
        if known.contains(&path) || !seen.insert(path.clone()) {
            out.push((path, None));
            continue;
        }
        progress(k + 1, n, &path);
        // Сбой разбора одного файла — ошибка этого файла, а не оборванное открытие всех.
        let wb = std::panic::catch_unwind(|| Workbook::open(&path))
            .map_err(|_| "сбой чтения файла".to_string())
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
        out.push((path, Some(wb)));
    }
    out
}

/// Сбой потока обработки (паника). Результаты пишутся во временные файлы и переименовываются только
/// в конце, а исходные файлы только читаются — поэтому их сбой не задевает.
fn crash_message(panic: Option<&str>) -> String {
    let why = panic.map(|p| format!(" (причина: {p})")).unwrap_or_default();
    format!("сбой обработки — программа прервала работу с файлом{why}. Исходные файлы не изменены.")
}

/// Паника внутри обработки — ошибка с её текстом, а не молча оборвавшийся поток.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
        Err(crash_message(text.as_deref()))
    })
}

/// Открытый файл: своя книга, свои правила. Генератор фейков и ключ — общие для всех файлов.
struct Doc {
    path: PathBuf,
    wb: Workbook,
    grids: HashMap<usize, Grid>,
    widths: HashMap<usize, Vec<f32>>,
    cur_sheet: usize,
    /// Правила этого файла (настройки общие — `App::settings`).
    profile: Profile,
    /// Галочка: файл обрабатывается. Снятая — файл остаётся в списке, но не обезличивается.
    on: bool,
}

struct App {
    docs: Vec<Doc>,
    cur: usize,
    settings: Settings,
    /// Профиль из командной строки — для первого файла без своего профиля.
    arg_profile: Option<Profile>,
    /// Начало протягивания выделения: колонка, строка.
    drag_anchor: Option<(u32, u32)>,
    /// Прокрутить таблицу к строке/колонке (после «Показать»).
    scroll_to: Option<(u32, u32)>,
    save_profile: bool,
    /// Выделение: колонка, строка от, строка до.
    sel: Option<(u32, u32, u32)>,
    range_text: String,
    new_kind: Kind,
    new_to_end: bool,
    key_in: Option<PathBuf>,
    /// Ключ выбран вручную — в него же и пишем (иначе — общий ключ папки первого файла).
    key_chosen: bool,
    save_key: bool,
    status: String,
    status_err: bool,
    log: Vec<String>,
    job: Option<Receiver<Msg>>,
    /// Флаг отмены идущей обработки: его поднимает закрытие окна с ответом «Прервать».
    cancel: Option<Arc<AtomicBool>>,
    /// Вопрос «Идёт обезличивание. Прервать?» на экране.
    close_ask: bool,
    /// Обработка прерывается по закрытию окна: окно закроется, когда поток уберёт временные файлы.
    closing: bool,
    /// Идущее фоновое открытие файлов.
    opening: Option<Receiver<OpenMsg>>,
    /// Ход идущей обработки и её начало; этап и строка «Файл k из n» — из журнала, для полосы хода.
    progress: Option<(Arc<Progress>, Instant)>,
    stage: String,
    file_line: String,
    /// Предпросмотр: по файлу — по правилу, и правила, по которым он посчитан (на больших файлах
    /// считается несколько секунд в фоне — пока правило не совпало, его старый предпросмотр не показывается).
    preview: Vec<Vec<RulePreview>>,
    preview_rules: Vec<Vec<Rule>>,
    preview_dirty: bool,
    preview_job: Option<Receiver<(Vec<Vec<Rule>>, Result<Vec<Vec<RulePreview>>, String>)>>,
    /// Активная ячейка (колонка, строка) — двигается стрелками.
    cursor: Option<(u32, u32)>,
    /// Видимая часть таблицы в прошлом кадре — чтобы прокрутить к активной ячейке.
    last_vp: Rect,
    ensure_visible: bool,
    grid_id: Option<egui::Id>,
    /// Правило под активной ячейкой — подсвечено справа; `scroll_rule` — прокрутить список к нему.
    focus_rule: Option<usize>,
    scroll_rule: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(bytes));
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                f.insert(0, "segoe".into());
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let mut app = App::blank();
        // finboard-anon.exe <файл.xlsx>… [профиль.json] — так же работает «Открыть с помощью».
        let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
        if let Some(p) = args.iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))) {
            match Profile::load(p) {
                Ok(pr) => {
                    app.settings = pr.settings.clone();
                    app.arg_profile = Some(pr);
                }
                Err(e) => app.set_status(format!("{e:#}"), true),
            }
        }
        let files: Vec<PathBuf> = args.into_iter().filter(|p| !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))).collect();
        app.open_files_bg(files, false, &cc.egui_ctx);
        app
    }

    fn blank() -> App {
        App {
            docs: Vec::new(),
            cur: 0,
            settings: Settings::default(),
            arg_profile: None,
            drag_anchor: None,
            scroll_to: None,
            save_profile: true,
            sel: None,
            range_text: String::new(),
            new_kind: Kind::FullName,
            new_to_end: false,
            key_in: None,
            key_chosen: false,
            save_key: true,
            status: "Откройте файлы Excel (.xlsx) кнопкой слева вверху или перетащите их в окно".into(),
            status_err: false,
            log: Vec::new(),
            job: None,
            cancel: None,
            close_ask: false,
            closing: false,
            opening: None,
            progress: None,
            stage: String::new(),
            file_line: String::new(),
            preview: Vec::new(),
            preview_rules: Vec::new(),
            preview_dirty: false,
            preview_job: None,
            cursor: None,
            last_vp: Rect::NOTHING,
            ensure_visible: false,
            grid_id: None,
            focus_rule: None,
            scroll_rule: false,
        }
    }

    fn set_status(&mut self, s: impl Into<String>, err: bool) {
        self.status = s.into();
        self.status_err = err;
    }

    fn doc(&self) -> Option<&Doc> {
        self.docs.get(self.cur)
    }

    /// Пути уже открытых файлов — их не нужно читать заново (при «закрыть прежние» — никаких).
    fn known_paths(&self, replace: bool) -> HashSet<PathBuf> {
        if replace {
            HashSet::new()
        } else {
            self.docs.iter().map(|d| d.path.clone()).collect()
        }
    }

    /// Открывает файлы сразу, в этом потоке (для тестов окна; само окно открывает в фоне — `open_files_bg`);
    /// `replace` — закрыть уже открытые (новый набор файлов).
    #[cfg(test)]
    fn open_files(&mut self, paths: Vec<PathBuf>, replace: bool) {
        if paths.is_empty() {
            return;
        }
        let loaded = load_books(paths, &self.known_paths(replace), &mut |_, _, _| {});
        self.add_books(loaded, replace);
    }

    /// Открывает файлы и папки в фоне: обход большой папки и чтение общих строк каждого файла занимают
    /// секунды и минуты — окно не замирает, в строке состояния — «Открытие файлов: k из n».
    fn open_files_bg(&mut self, paths: Vec<PathBuf>, replace: bool, ctx: &egui::Context) {
        if paths.is_empty() {
            return;
        }
        if self.opening.is_some() {
            self.set_status("Дождитесь: предыдущие файлы ещё открываются", true);
            return;
        }
        let known = self.known_paths(replace);
        let (tx, rx) = channel();
        self.opening = Some(rx);
        self.set_status("Открытие файлов…", false);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut files = Vec::new();
            let mut empty = Vec::new();
            for p in paths {
                if p.is_dir() {
                    let found = excel_files_in(&p);
                    if found.is_empty() {
                        empty.push(p);
                    }
                    files.extend(found);
                } else {
                    files.push(p);
                }
            }
            let loaded = load_books(files, &known, &mut |k, n, p| {
                let _ = tx.send(OpenMsg::Progress(format!("Открытие файлов: {k} из {n} — {}", file_name(p))));
                ctx.request_repaint();
            });
            let _ = tx.send(OpenMsg::Done(loaded, replace, empty));
            ctx.request_repaint();
        });
    }

    fn poll_open(&mut self) {
        let Some(rx) = &self.opening else { return };
        let mut msgs = Vec::new();
        let mut gone = false;
        loop {
            match rx.try_recv() {
                Ok(m) => msgs.push(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        for m in msgs {
            match m {
                OpenMsg::Progress(s) => self.set_status(s, false),
                OpenMsg::Done(loaded, replace, empty) => {
                    self.opening = None;
                    if loaded.is_empty() {
                        let dirs: Vec<String> = empty.iter().map(|d| d.display().to_string()).collect();
                        self.set_status(format!("В папке {} нет файлов Excel", dirs.join(", ")), true);
                    } else {
                        self.add_books(loaded, replace);
                    }
                    return;
                }
            }
        }
        if gone {
            self.opening = None;
            self.set_status("Сбой при открытии файлов — откройте их ещё раз", true);
        }
    }

    /// Прочитанные книги — в список файлов: профиль рядом с файлом, ключ папки, строка состояния.
    fn add_books(&mut self, loaded: Vec<Loaded>, replace: bool) {
        if replace {
            self.docs.clear();
            self.key_in = None;
            self.key_chosen = false;
        }
        let mut notes: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut last = None;
        for (path, wb) in loaded {
            if let Some(i) = self.docs.iter().position(|d| d.path == path) {
                last = Some(i);
                continue;
            }
            let wb = match wb {
                Some(Ok(wb)) => wb,
                Some(Err(e)) => {
                    errors.push(format!("{}: {e}", file_name(&path)));
                    continue;
                }
                // Был открыт, когда открытие началось, а теперь закрыт — его не читали; не открываем.
                None => continue,
            };
            let first = self.docs.is_empty();
            let mut profile = Profile::default();
            let side = side_profile_path(&path);
            if side.exists() {
                match Profile::load(&side) {
                    Ok(p) => {
                        notes.push(format!("{}: профиль загружен (правил {})", file_name(&path), p.rules.len()));
                        if first && self.arg_profile.is_none() {
                            self.settings = p.settings.clone();
                        }
                        profile = p;
                    }
                    Err(e) => errors.push(format!("{}: профиль не читается: {e:#}", file_name(&path))),
                }
            } else if let Some(p) = self.arg_profile.take() {
                profile = p;
            }
            let missing: Vec<&str> = profile.rules.iter().filter(|r| wb.sheet_index(&r.sheet).is_none()).map(|r| r.sheet.as_str()).collect();
            if !missing.is_empty() {
                errors.push(format!("{}: нет листов из профиля: {}", file_name(&path), missing.join(", ")));
            }
            match (find_key(&path), &self.key_in) {
                (Some(k), None) => {
                    notes.push(format!("ключ подключён ({}) — те же люди и организации получат те же фейки", file_name(&k)));
                    self.key_in = Some(k);
                }
                (Some(k), Some(cur)) if &k != cur => {
                    errors.push(format!("у {} есть свой ключ {} — используется общий {}", file_name(&path), k.display(), file_name(cur)));
                }
                _ => {}
            }
            self.docs.push(Doc { path, wb, grids: HashMap::new(), widths: HashMap::new(), cur_sheet: 0, profile, on: true });
            last = Some(self.docs.len() - 1);
        }
        if let Some(i) = last {
            self.select_doc(i);
        }
        self.preview_dirty = true;
        let opened = match self.doc() {
            Some(d) if self.docs.len() == 1 => format!("Открыт: {} — листов: {}", d.path.display(), d.wb.sheets.len()),
            _ => format!("Открыто файлов: {}", self.docs.len()),
        };
        if !errors.is_empty() {
            self.set_status(format!("{opened}. Внимание: {}", errors.join("; ")), true);
        } else if notes.is_empty() {
            self.set_status(opened, false);
        } else {
            self.set_status(format!("{opened} — {}", notes.join("; ")), false);
        }
    }

    fn select_doc(&mut self, i: usize) {
        self.cur = i;
        self.sel = None;
        self.cursor = None;
        self.drag_anchor = None;
        self.focus_rule = None;
        let s = self.docs[i].cur_sheet;
        self.ensure_grid(s);
    }

    fn close_doc(&mut self, i: usize) {
        self.docs.remove(i);
        if self.docs.is_empty() {
            self.key_in = None;
            self.key_chosen = false;
            self.cur = 0;
        } else {
            self.select_doc(self.cur.min(self.docs.len() - 1));
        }
        self.preview_dirty = true;
    }

    fn ensure_grid(&mut self, idx: usize) {
        let Some(doc) = self.docs.get_mut(self.cur) else { return };
        if doc.grids.contains_key(&idx) {
            return;
        }
        match load_grid(&doc.path, &doc.wb, idx, GRID_ROWS) {
            Ok(g) => {
                doc.widths.insert(idx, column_widths(&g));
                doc.grids.insert(idx, g);
            }
            Err(e) => self.set_status(format!("Лист не читается: {e:#}"), true),
        }
    }

    fn sheet_name(&self, idx: usize) -> String {
        self.doc().and_then(|d| d.wb.sheets.get(idx)).map(|s| s.name.clone()).unwrap_or_default()
    }

    /// Подпись шаблона из заголовка над диапазоном (или из шапки, попавшей в диапазон). Сообщение — для строки состояния.
    fn fill_template(&mut self, rule: &mut Rule) -> Option<String> {
        let idx = self.doc()?.wb.sheet_index(&rule.sheet)?;
        self.ensure_grid(idx);
        detect::fill_template(rule, self.docs.get(self.cur)?.grids.get(&idx)?)
    }

    fn add_rules(&mut self, rules: Vec<Rule>, replace: bool) -> (usize, usize) {
        self.preview_dirty = true;
        match self.docs.get_mut(self.cur) {
            Some(d) => d.profile.add_rules(rules, replace),
            None => (0, 0),
        }
    }

    fn auto_detect(&mut self, all: bool) {
        let Some(doc) = self.doc() else { return };
        let idxs: Vec<usize> = if all { (0..doc.wb.sheets.len()).collect() } else { vec![doc.cur_sheet] };
        let mut found = Vec::new();
        for i in idxs {
            self.ensure_grid(i);
            let name = self.sheet_name(i);
            if let Some(g) = self.docs[self.cur].grids.get(&i) {
                found.extend(detect::suggest(&name, g));
            }
        }
        let total = found.len();
        let (added, _) = self.add_rules(found, false);
        self.set_status(
            format!("Автоподбор: найдено {total}, добавлено {added}. Проверьте диапазоны и типы в списке правил справа"),
            false,
        );
    }

    /// Предпросмотр считается тем же кодом, что и обработка (`anon::preview_batch`): все файлы, тот же ключ.
    /// В фоне: файл в сотни тысяч строк читается секунды, окно не должно замирать на каждой правке.
    fn start_preview(&mut self, ctx: &egui::Context) {
        self.preview_dirty = false;
        if self.docs.is_empty() {
            self.preview.clear();
            self.preview_rules.clear();
            return;
        }
        // Неотмеченный файл — без правил: в обработку он не идёт, значения из него не считаются.
        let files: Vec<(PathBuf, Vec<Rule>)> =
            self.docs.iter().map(|d| (d.path.clone(), if d.on { d.profile.rules.clone() } else { Vec::new() })).collect();
        let rules: Vec<Vec<Rule>> = files.iter().map(|(_, r)| r.clone()).collect();
        let (settings, key) = (self.settings.clone(), self.key_in.clone());
        let (tx, rx) = channel();
        self.preview_job = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = preview_batch(&files, &settings, key.as_deref()).map_err(|e| format!("{e:#}"));
            let _ = tx.send((rules, r));
            ctx.request_repaint();
        });
    }

    fn poll_preview(&mut self) {
        let Some(rx) = &self.preview_job else { return };
        match rx.try_recv() {
            Ok((rules, r)) => {
                self.preview_job = None;
                match r {
                    Ok(p) => {
                        self.preview = p;
                        self.preview_rules = rules;
                    }
                    Err(e) => {
                        self.preview.clear();
                        self.preview_rules.clear();
                        self.set_status(format!("Предпросмотр: {e}"), true);
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.preview_job = None,
        }
    }

    /// Строка, после которой правило считается «до конца»: последняя строка листа в просмотре.
    fn sheet_last_row(&self, sheet: &str) -> Option<u32> {
        let d = self.doc()?;
        d.grids.get(&d.wb.sheet_index(sheet)?).map(|g| g.max_row)
    }

    fn poll_job(&mut self) {
        let Some(rx) = &self.job else { return };
        let mut finished = None;
        loop {
            let m = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                // Поток обработки кончился, не прислав итога (сбой): иначе окно навсегда осталось бы в «Обработка…».
                Err(TryRecvError::Disconnected) => {
                    finished = finished.or_else(|| Some((Err(crash_message(None)), Vec::new())));
                    break;
                }
            };
            match m {
                Msg::Log(s) => {
                    if s.starts_with("Файл ") && s.contains(" из ") {
                        self.file_line = s.clone();
                        self.stage.clear();
                    } else if let Some(st) = stage_of(&s) {
                        self.stage = st.into();
                    }
                    self.log.push(s);
                }
                Msg::Done(r, p) => finished = Some((r, p)),
            }
        }
        if let Some((r, profiles)) = finished {
            self.job = None;
            self.cancel = None;
            let took = self.progress.take().map(|(_, t0)| format!(" — за {}", format_duration(t0.elapsed()))).unwrap_or_default();
            match r {
                Ok(reports) => {
                    let many = reports.len() > 1;
                    for (rep, p) in reports.iter().zip(&profiles) {
                        if many {
                            self.log.push(String::new());
                        }
                        self.log.extend(rep.summary(p));
                    }
                    // Только что записанный ключ — источник фейков для следующего прогона и предпросмотра.
                    if let Some(k) = reports.first().and_then(|r| r.key_path.clone()) {
                        self.key_in = Some(k);
                        self.preview_dirty = true;
                    }
                    let leaks = reports.iter().filter(|r| !r.leaks.is_empty()).count();
                    if leaks > 0 {
                        self.set_status(format!("Готово, но проверка нашла остатки реальных значений — см. журнал внизу{took}"), true);
                    } else if many {
                        let dir = reports[0].output.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        self.set_status(format!("Готово: файлов {} в {dir}{took}", reports.len()), false);
                    } else {
                        self.set_status(format!("Готово: {}{took}", reports[0].output.display()), false);
                    }
                }
                Err(e) => {
                    self.log.push(format!("Ошибка: {e}"));
                    self.set_status(format!("Ошибка: {e}"), true);
                }
            }
        }
    }

    fn start_job(&mut self, ctx: &egui::Context) {
        let inputs: Vec<PathBuf> = self.docs.iter().filter(|d| d.on).map(|d| d.path.clone()).collect();
        if inputs.is_empty() {
            return;
        }
        let empty: Vec<String> = self.docs.iter().filter(|d| d.on && d.profile.rules.is_empty()).map(|d| file_name(&d.path)).collect();
        if !empty.is_empty() {
            self.set_status(format!("Нет правил в файлах: {} — добавьте правила или снимите с них галочку", empty.join(", ")), true);
            return;
        }
        let outputs = if inputs.len() == 1 {
            let default = default_output_path(&inputs[0]);
            let mut dlg = rfd::FileDialog::new().add_filter("Excel", &["xlsx", "xlsm"]);
            if let Some(dir) = default.parent() {
                dlg = dlg.set_directory(dir);
            }
            if let Some(name) = default.file_name().and_then(|n| n.to_str()) {
                dlg = dlg.set_file_name(name);
            }
            let Some(output) = dlg.save_file() else { return };
            vec![output]
        } else {
            let mut dlg = rfd::FileDialog::new().set_title("Папка для обезличенных файлов");
            if let Some(dir) = inputs[0].parent() {
                dlg = dlg.set_directory(dir);
            }
            let Some(dir) = dlg.pick_folder() else { return };
            batch_outputs(&inputs, Some(&dir))
        };
        self.log.clear();
        let profiles: Vec<Profile> =
            self.docs.iter().filter(|d| d.on).map(|d| Profile { rules: d.profile.rules.clone(), settings: self.settings.clone() }).collect();
        if self.save_profile {
            for (d, p) in self.docs.iter().filter(|d| d.on).zip(&profiles) {
                let side = side_profile_path(&d.path);
                match p.save(&side) {
                    Ok(()) => self.log.push(format!("Профиль сохранён рядом с файлом: {}", side.display())),
                    Err(e) => self.log.push(format!("Внимание: профиль не сохранён: {e:#}")),
                }
            }
        }
        let key_out = key_out_path(&inputs[0], self.key_in.as_deref(), self.key_chosen);
        let batch = Batch {
            files: inputs.iter().zip(outputs).zip(&profiles).map(|((i, o), p)| FileJob { input: i.clone(), output: o, rules: p.rules.clone() }).collect(),
            settings: self.settings.clone(),
            key_in: self.key_in.clone(),
            key_out: self.save_key.then_some(key_out),
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.job = Some(rx);
        let progress = Arc::new(Progress::default());
        self.progress = Some((progress.clone(), Instant::now()));
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.stage.clear();
        self.file_line.clear();
        self.set_status("Обработка…", false);
        std::thread::spawn(move || {
            // Счётчик — в потоке обработки: считаются только его чтения книг.
            finboard_anon::xlsx::track_progress(Some(progress));
            finboard_anon::anon::track_cancel(Some(cancel));
            let tx_log = tx.clone();
            let ctx_log = ctx.clone();
            let res = guarded(|| {
                run_batch(&batch, &mut |m| {
                    let _ = tx_log.send(Msg::Log(m));
                    ctx_log.request_repaint();
                })
                .map_err(|e| format!("{e:#}"))
            });
            let _ = tx.send(Msg::Done(res, profiles));
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------ панели

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let idle = self.opening.is_none();
            if ui.add_enabled(idle, egui::Button::new("Открыть Excel…")).on_hover_text("Можно выбрать сразу несколько файлов одного заказчика").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Excel", &["xlsx", "xlsm"]).pick_files() {
                    self.open_files_bg(p, true, ui.ctx());
                }
            }
            if ui
                .add_enabled(idle && !self.docs.is_empty(), egui::Button::new("Добавить файл…"))
                .on_hover_text("Ещё файл в тот же набор: одни и те же люди, организации и ИНН получат одни и те же фейки во всех файлах")
                .clicked()
            {
                if let Some(p) = rfd::FileDialog::new().add_filter("Excel", &["xlsx", "xlsm"]).pick_files() {
                    self.open_files_bg(p, false, ui.ctx());
                }
            }
            if ui
                .add_enabled(idle, egui::Button::new("Добавить папку…"))
                .on_hover_text("Все файлы Excel из папки и её подпапок (кроме уже обезличенных «…_anon»). Лишние — снять галочку или убрать из списка")
                .clicked()
            {
                if let Some(dir) = rfd::FileDialog::new().set_title("Папка с файлами Excel").pick_folder() {
                    self.open_files_bg(vec![dir], false, ui.ctx());
                }
            }
            ui.separator();
            let has_doc = !self.docs.is_empty();
            if ui
                .add_enabled(has_doc, egui::Button::new("Загрузить профиль…"))
                .on_hover_text("Правила из JSON для текущего файла — для повторной обработки новых версий файла")
                .clicked()
            {
                if let Some(p) = rfd::FileDialog::new().add_filter("Профиль", &["json"]).pick_file() {
                    match Profile::load(&p) {
                        Ok(pr) => {
                            self.settings = pr.settings.clone();
                            let n = pr.rules.len();
                            self.docs[self.cur].profile = pr;
                            self.preview_dirty = true;
                            self.set_status(format!("Профиль загружен: правил {n}"), false);
                        }
                        Err(e) => self.set_status(format!("{e:#}"), true),
                    }
                }
            }
            if ui.add_enabled(has_doc, egui::Button::new("Сохранить профиль…")).clicked() {
                let path = self.docs[self.cur].path.clone();
                let name = side_profile_path(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut dlg = rfd::FileDialog::new().add_filter("Профиль", &["json"]).set_file_name(name);
                if let Some(dir) = path.parent() {
                    dlg = dlg.set_directory(dir);
                }
                if let Some(p) = dlg.save_file() {
                    let pr = Profile { rules: self.docs[self.cur].profile.rules.clone(), settings: self.settings.clone() };
                    match pr.save(&p) {
                        Ok(()) => self.set_status(format!("Профиль сохранён: {}", p.display()), false),
                        Err(e) => self.set_status(format!("{e:#}"), true),
                    }
                }
            }
            ui.separator();
            if !idle {
                ui.spinner();
            }
            let color = if self.status_err { Color32::from_rgb(220, 80, 60) } else { ui.visuals().text_color() };
            ui.label(RichText::new(&self.status).color(color));
        });
    }

    fn sheets_panel(&mut self, ui: &mut egui::Ui) {
        if self.docs.is_empty() {
            ui.heading("Файлы");
            ui.label("Файл не открыт");
            return;
        }
        let mut pick = None;
        let mut close = None;
        // Высота списка файлов меняется перетаскиванием границы между «Файлами» и «Листами».
        let max_files = (ui.available_height() - 160.0).max(80.0);
        egui::TopBottomPanel::top("files_panel")
            .resizable(true)
            .default_height(180.0)
            .height_range(70.0..=max_files)
            .show_inside(ui, |ui| {
                let on = self.docs.iter().filter(|d| d.on).count();
                ui.horizontal(|ui| {
                    ui.heading(format!("Файлы ({on} из {})", self.docs.len()));
                    if self.docs.len() > 1 {
                        let mut all = on == self.docs.len();
                        if ui.checkbox(&mut all, "все").on_hover_text("Отметить или снять все файлы").changed() {
                            self.docs.iter_mut().for_each(|d| d.on = all);
                            self.preview_dirty = true;
                        }
                    }
                });
                ui.label(RichText::new("Галочка — файл обрабатывается. Один человек, организация, ИНН — один фейк во всех отмеченных файлах").small().weak());
                egui::ScrollArea::vertical().id_salt("files").auto_shrink([false, false]).show(ui, |ui| {
                    for (i, d) in self.docs.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut d.on, "").on_hover_text("Обрабатывать этот файл").changed() {
                                self.preview_dirty = true;
                            }
                            let label = format!("{}  [{}]", file_name(&d.path), d.profile.rules.len());
                            let text = if d.on { RichText::new(label) } else { RichText::new(label).weak().strikethrough() };
                            if ui.selectable_label(self.cur == i, text).on_hover_text(d.path.display().to_string()).clicked() {
                                pick = Some(i);
                            }
                            if ui.small_button("убрать").on_hover_text("Убрать файл из списка (сам файл не удаляется)").clicked() {
                                close = Some(i);
                            }
                        });
                    }
                });
            });
        if let Some(i) = close {
            self.close_doc(i);
            return;
        }
        if let Some(i) = pick {
            self.select_doc(i);
        }
        ui.heading("Листы");
        let doc = &self.docs[self.cur];
        let names: Vec<String> = doc.wb.sheets.iter().map(|s| s.name.clone()).collect();
        let mut clicked = None;
        egui::ScrollArea::vertical().id_salt(("sheets", self.cur)).max_height(ui.available_height() - 80.0).show(ui, |ui| {
            for (i, n) in names.iter().enumerate() {
                let cnt = doc.profile.rules.iter().filter(|r| &r.sheet == n).count();
                let label = if cnt > 0 { format!("{n}  [{cnt}]") } else { n.clone() };
                if ui.selectable_label(doc.cur_sheet == i, label).clicked() {
                    clicked = Some(i);
                }
            }
        });
        if let Some(i) = clicked {
            self.docs[self.cur].cur_sheet = i;
            self.sel = None;
            self.cursor = None;
            self.focus_rule = None;
            self.ensure_grid(i);
        }
        ui.separator();
        if ui.button("Автоподбор: этот лист").on_hover_text("Ищет заголовки вроде «ФИО», «Телефон», «ИНН», «Контрагент» и предлагает диапазоны под ними").clicked() {
            self.auto_detect(false);
        }
        if ui.button("Автоподбор: все листы").clicked() {
            self.auto_detect(true);
        }
    }

    fn grid_panel(&mut self, ui: &mut egui::Ui) {
        if self.docs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Откройте .xlsx или перетащите файлы в окно").size(18.0));
            });
            return;
        }
        let cur_sheet = self.docs[self.cur].cur_sheet;
        let sheet = self.sheet_name(cur_sheet);

        // Строка добавления правила
        ui.horizontal(|ui| {
            ui.label("Диапазон:");
            ui.add(egui::TextEdit::singleline(&mut self.range_text).id(egui::Id::new("range_text")).desired_width(110.0).hint_text("C5:C120"))
                .on_hover_text("Например C5:C120, C5:C (до конца листа), B5:D100 (несколько колонок).\nМожно выделить мышью: протянуть по ячейкам, или щелчок — начало, Shift+щелчок — конец; щелчок по букве колонки — вся колонка.\nЕсли диапазон пересекается с правилом — правило заменяется новым");
            ui.checkbox(&mut self.new_to_end, "до конца листа");
            egui::ComboBox::from_id_salt("new_kind")
                .selected_text(self.new_kind.title())
                .width(230.0)
                .show_ui(ui, |ui| {
                    for k in Kind::ALL {
                        ui.selectable_value(&mut self.new_kind, k, k.title()).on_hover_text(k.hint());
                    }
                });
            if ui.button("Добавить правило").clicked() {
                match parse_range(&self.range_text) {
                    Some((c1, c2, r1, r2)) => {
                        // Выделение до последней строки листа — правило «до конца»: в настоящем файле строк больше.
                        let last = self.sheet_last_row(&sheet);
                        let to_end = self.new_to_end || r2.zip(last).is_some_and(|(b, l)| b >= l);
                        let to = if to_end { None } else { r2 };
                        let mut rules: Vec<Rule> = (c1..=c2).map(|c| Rule::new(&sheet, c, r1, to, self.new_kind)).collect();
                        let notes: Vec<String> = rules.iter_mut().filter_map(|r| self.fill_template(r)).collect();
                        let (added, replaced) = self.add_rules(rules, true);
                        let mut msg = match (added, replaced) {
                            (a, 0) => format!("Добавлено правил: {a}"),
                            (0, r) => format!("Правило заменено новым диапазоном (было пересекающихся: {r})"),
                            (a, r) => format!("Добавлено правил: {a}, заменено: {r}"),
                        };
                        if to_end && !self.new_to_end {
                            msg = format!("{msg} — до конца листа (выделение дошло до последней строки)");
                        }
                        if let Some(n) = notes.first() {
                            msg = format!("{msg}. {n}");
                        } else if self.new_kind == Kind::Text {
                            msg = format!("{msg}. Над диапазоном нет заголовка — впишите подпись в правиле справа (например «Налог»), иначе будет «Значение 1, 2…»");
                        }
                        self.set_status(msg, false);
                    }
                    None => self.set_status("Не понял диапазон. Пример: C5:C120 или C5:C", true),
                }
            }
        });
        let doc = &self.docs[self.cur];
        if let Some(g) = doc.grids.get(&cur_sheet) {
            let mut info = format!("{} — лист «{sheet}»: строк {}, колонок {}", file_name(&doc.path), g.max_row, g.max_col + 1);
            if let Some((c, a, b)) = self.sel {
                info.push_str(&format!(". Выделено {0}{1}:{0}{2} — строк {3}", col_name(c), a, b, b - a + 1));
            }
            if g.truncated {
                let all = g.dim_rows.map(|n| format!(" из {n}")).unwrap_or_default();
                info.push_str(&format!(" (показаны первые {GRID_ROWS}{all}; правила «до конца» действуют на весь лист)"));
            }
            ui.label(RichText::new(info).weak());
        }
        ui.separator();

        let Some(grid) = doc.grids.get(&cur_sheet) else {
            ui.label("Лист не загружен");
            return;
        };
        let widths = doc.widths.get(&cur_sheet).cloned().unwrap_or_default();
        let ncols = widths.len();
        let nrows = grid.max_row;
        // Левая граница каждой колонки (без колонки номеров строк) и общая ширина.
        let mut xs = Vec::with_capacity(ncols + 1);
        let mut acc = 0.0;
        for w in &widths {
            xs.push(acc);
            acc += w;
        }
        xs.push(acc);
        let rules: Vec<&Rule> = doc.profile.rules.iter().filter(|r| r.sheet == sheet).collect();
        let sel = self.sel;
        let visuals = ui.visuals().clone();
        let rule_fill = Color32::from_rgba_unmultiplied(70, 130, 255, 60);
        let sel_fill = Color32::from_rgba_unmultiplied(255, 165, 0, 120);
        let line = Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.5));
        let font = FontId::proportional(13.0);
        let head_font = FontId::proportional(16.0);
        let (head_fill, head_text, cur_fill) = if visuals.dark_mode {
            (Color32::from_rgb(40, 62, 98), Color32::WHITE, Color32::from_rgb(200, 120, 0))
        } else {
            (Color32::from_rgb(200, 216, 242), Color32::BLACK, Color32::from_rgb(255, 190, 90))
        };
        let head_line = Stroke::new(1.0_f32, head_text.gamma_multiply(0.35));
        let shift = ui.input(|i| i.modifiers.shift);
        let (old, old_cursor) = (self.sel, self.cursor);
        let press_origin = ui.input(|i| i.pointer.press_origin());

        // Клавиатура как в Excel: стрелки, Shift — выделение вверх-вниз, Ctrl — к краю данных,
        // PageUp/PageDown, Home, Ctrl+Home/End. Не мешаем вводу в полях и открытым спискам.
        // (`wants_keyboard_input` не годится: он истинен при любом фокусе, в том числе у самой таблицы.)
        let focused = ui.memory(|m| m.focused());
        let typing = focused.is_some_and(|f| Some(f) != self.grid_id && egui::TextEdit::load_state(ui.ctx(), f).is_some());
        let popup = ui.memory(|m| m.any_popup_open());
        if !typing && !popup && ncols > 0 && nrows > 0 {
            let keys: Vec<(egui::Key, egui::Modifiers)> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            let (max_c, max_r) = (ncols as u32 - 1, nrows);
            let page = ((self.last_vp.height() - HEAD_H) / ROW_H).max(1.0) as u32;
            let mut cur = self.cursor.or(sel.map(|(c, a, _)| (c, a))).unwrap_or((0, 1));
            let mut moved = None;
            for (key, m) in keys {
                use egui::Key::*;
                let dir = match key {
                    ArrowUp => Some((0, -1)),
                    ArrowDown => Some((0, 1)),
                    ArrowLeft => Some((-1, 0)),
                    ArrowRight => Some((1, 0)),
                    _ => None,
                };
                let next = match (key, dir) {
                    (_, Some(d)) if m.command => Some(grid.jump(cur, d, max_c, max_r)),
                    (_, Some((dc, dr))) => Some((
                        (cur.0 as i64 + dc).clamp(0, max_c as i64) as u32,
                        (cur.1 as i64 + dr).clamp(1, max_r as i64) as u32,
                    )),
                    (PageDown, _) => Some((cur.0, (cur.1 + page).min(max_r))),
                    (PageUp, _) => Some((cur.0, cur.1.saturating_sub(page).max(1))),
                    (Home, _) if m.command => Some((0, 1)),
                    (Home, _) => Some((0, cur.1)),
                    (End, _) if m.command => Some((max_c, max_r)),
                    _ => None,
                };
                if let Some(n) = next {
                    cur = n;
                    moved = Some((cur, m.shift));
                }
            }
            if let Some(((c, r), extend)) = moved {
                self.cursor = Some((c, r));
                match self.drag_anchor {
                    Some((ac, a)) if extend && ac == c => self.sel = Some((c, a.min(r), a.max(r))),
                    _ => {
                        self.drag_anchor = Some((c, r));
                        self.sel = Some((c, r, r));
                    }
                }
                self.ensure_visible = true;
                // Стрелки дальше достаются таблице, а не переводят фокус между кнопками.
                if let Some(id) = self.grid_id {
                    ui.memory_mut(|m| m.request_focus(id));
                }
            }
        }
        let sel = self.sel;

        let mut area = egui::ScrollArea::both()
            .id_salt(("grid", self.cur, cur_sheet))
            .auto_shrink([false, false])
            .drag_to_scroll(false);
        if let Some((r, c)) = self.scroll_to.take() {
            let x = xs.get(c as usize).copied().unwrap_or(0.0);
            area = area.scroll_offset(vec2((x - 40.0).max(0.0), ((r as f32 - 4.0) * ROW_H).max(0.0)));
        } else if std::mem::take(&mut self.ensure_visible) {
            // Активная ячейка за краем видимой части — прокрутить ровно настолько, чтобы она показалась.
            if let (Some((c, r)), true) = (self.cursor, self.last_vp.is_positive() && ncols > 0) {
                let vp = self.last_vp;
                let c = (c as usize).min(ncols - 1);
                let mut off = vp.min;
                if xs[c] < vp.min.x {
                    off.x = xs[c];
                } else if ROWNUM_W + xs[c + 1] > vp.max.x {
                    off.x = (ROWNUM_W + xs[c + 1] - vp.width()).min(xs[c]);
                }
                let (y0, y1) = ((r - 1) as f32 * ROW_H, HEAD_H + r as f32 * ROW_H);
                if y0 < vp.min.y {
                    off.y = y0;
                } else if y1 > vp.max.y {
                    off.y = y1 - vp.height();
                }
                if off != vp.min {
                    area = area.scroll_offset(off.to_vec2());
                }
            }
        }
        let mut clicked = Hit::None;
        let mut double = Hit::None;
        let mut drag_start = Hit::None;
        let mut drag_to = Hit::None;
        let mut seen_vp = Rect::NOTHING;
        let mut grid_id = None;
        let cursor = self.cursor;
        area.show_viewport(ui, |ui, vp| {
            seen_vp = vp;
            let (rect, resp) = ui.allocate_exact_size(vec2(ROWNUM_W + acc, HEAD_H + nrows as f32 * ROW_H), Sense::click_and_drag());
            grid_id = Some(resp.id);
            if resp.clicked() || resp.drag_started() {
                resp.request_focus();
            }
            // Стрелки остаются таблице, а не переводят фокус на соседние кнопки.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(resp.id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() })
            });
            let o = rect.min;
            let vis = Rect::from_min_size(o + vp.min.to_vec2(), vp.size());
            let painter = ui.painter().clone();
            let r0 = ((vp.min.y - HEAD_H) / ROW_H).floor().max(0.0) as u32;
            let r1 = (((vp.max.y - HEAD_H) / ROW_H).ceil().max(0.0) as u32).min(nrows);
            let c0 = xs.partition_point(|&x| x <= vp.min.x - ROWNUM_W).saturating_sub(1);
            let c1 = xs.partition_point(|&x| x < vp.max.x - ROWNUM_W).min(ncols);

            // Ячейки (только видимые).
            for ri in r0..r1 {
                let row = ri + 1;
                let y = o.y + HEAD_H + ri as f32 * ROW_H;
                if row % 2 == 0 {
                    painter.rect_filled(Rect::from_min_size(pos2(vis.min.x, y), vec2(vis.width(), ROW_H)), 0.0, visuals.faint_bg_color);
                }
                for c in c0..c1 {
                    let col = c as u32;
                    let x = o.x + ROWNUM_W + xs[c];
                    let cr = Rect::from_min_size(pos2(x, y), vec2(widths[c], ROW_H));
                    if sel.is_some_and(|(sc, a, b)| sc == col && row >= a && row <= b) {
                        painter.rect_filled(cr, 0.0, sel_fill);
                    } else if rules.iter().any(|x| x.covers(row, col)) {
                        painter.rect_filled(cr, 0.0, rule_fill);
                    }
                    if cursor == Some((col, row)) {
                        painter.rect_stroke(cr.shrink(1.0), 0.0, Stroke::new(2.0_f32, Color32::from_rgb(30, 120, 60)));
                    }
                    if let Some(cell) = grid.get(row, col) {
                        let color = if cell.formula { visuals.weak_text_color() } else { visuals.text_color() };
                        painter.with_clip_rect(cr.shrink(1.0)).text(
                            pos2(x + 4.0, y + ROW_H / 2.0),
                            Align2::LEFT_CENTER,
                            cell_display(cell),
                            font.clone(),
                            color,
                        );
                    }
                    painter.vline(cr.max.x, cr.y_range(), line);
                }
                painter.hline(vis.x_range(), y + ROW_H, line);
            }

            // Закреплённая строка с буквами колонок: своя заливка и крупные буквы — колонку видно сразу.
            painter.rect_filled(Rect::from_min_size(vis.min, vec2(vis.width(), HEAD_H)), 0.0, head_fill);
            for c in c0..c1 {
                let col = c as u32;
                let cr = Rect::from_min_size(pos2(o.x + ROWNUM_W + xs[c], vis.min.y), vec2(widths[c], HEAD_H));
                if cursor.is_some_and(|(cc, _)| cc == col) {
                    painter.rect_filled(cr, 0.0, cur_fill);
                } else if sel.is_some_and(|(sc, _, _)| sc == col) {
                    painter.rect_filled(cr, 0.0, sel_fill);
                }
                painter.text(cr.center(), Align2::CENTER_CENTER, col_name(col), head_font.clone(), head_text);
                painter.vline(cr.max.x, cr.y_range(), head_line);
            }
            painter.hline(vis.x_range(), vis.min.y + HEAD_H, head_line);

            // Закреплённая колонка с номерами строк.
            let nums = Rect::from_min_size(pos2(vis.min.x, vis.min.y + HEAD_H), vec2(ROWNUM_W, (vis.height() - HEAD_H).max(0.0)));
            painter.rect_filled(nums, 0.0, head_fill);
            let np = painter.with_clip_rect(nums);
            for ri in r0..r1 {
                let row = ri + 1;
                let rr = Rect::from_min_size(pos2(vis.min.x, o.y + HEAD_H + ri as f32 * ROW_H), vec2(ROWNUM_W, ROW_H));
                if cursor.is_some_and(|(_, cr)| cr == row) {
                    np.rect_filled(rr, 0.0, cur_fill);
                } else if sel.is_some_and(|(_, a, b)| row >= a && row <= b) {
                    np.rect_filled(rr, 0.0, sel_fill);
                }
                np.text(pos2(rr.max.x - 6.0, rr.center().y), Align2::RIGHT_CENTER, row.to_string(), font.clone(), head_text);
            }
            painter.vline(vis.min.x + ROWNUM_W, vis.y_range(), head_line);
            // Угол — адрес активной ячейки, как поле имени в Excel.
            let corner = Rect::from_min_size(vis.min, vec2(ROWNUM_W, HEAD_H));
            painter.rect_filled(corner, 0.0, head_fill);
            if let Some((c, r)) = cursor {
                painter.with_clip_rect(corner).text(corner.center(), Align2::CENTER_CENTER, format!("{}{r}", col_name(c)), FontId::proportional(12.0), head_text);
            }

            let hit = |p: egui::Pos2| -> Hit {
                if !vis.contains(p) || p.x < vis.min.x + ROWNUM_W {
                    return Hit::None;
                }
                let cx = p.x - o.x - ROWNUM_W;
                let Some(col) = xs.partition_point(|&x| x <= cx).checked_sub(1).filter(|&c| c < ncols) else {
                    return Hit::None;
                };
                if p.y < vis.min.y + HEAD_H {
                    return Hit::Column(col as u32);
                }
                let row = ((p.y - o.y - HEAD_H) / ROW_H).floor() as i64 + 1;
                if row >= 1 && row <= nrows as i64 {
                    Hit::Cell(row as u32, col as u32)
                } else {
                    Hit::None
                }
            };
            if resp.drag_started() {
                if let Some(p) = press_origin {
                    drag_start = hit(p);
                }
            }
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    // При протягивании за край — ограничиваем видимой областью.
                    drag_to = hit(p.clamp(vis.min + vec2(ROWNUM_W + 1.0, HEAD_H + 1.0), vis.max - vec2(1.0, 1.0)));
                }
            }
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    clicked = hit(p);
                }
            }
            if resp.double_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    double = hit(p);
                }
            }
            if let Some(p) = resp.hover_pos() {
                match hit(p) {
                    Hit::Cell(r, c) => {
                        let mut tip = format!("{}{}", col_name(c), r);
                        if let Some(cell) = grid.get(r, c) {
                            tip.push_str(&format!(": {}", cell_display(cell)));
                        }
                        if let Some(x) = rules.iter().find(|x| x.covers(r, c)) {
                            tip.push_str(&format!("\nПравило {} — {} (щелчок — правило справа)", x.range_text(), x.kind.title()));
                        }
                        tip.push_str("\nДвойной щелчок — колонка отсюда до конца листа");
                        resp.on_hover_text_at_pointer(tip);
                    }
                    Hit::Column(_) => {
                        resp.on_hover_text_at_pointer("Щелчок — вся колонка под шапкой, до конца листа");
                    }
                    Hit::None => {}
                }
            }
        });

        self.last_vp = seen_vp;
        self.grid_id = grid_id;
        // Колонка целиком (щелчок по букве) или двойной щелчок по ячейке — от неё (по шапке — от строки
        // под шапкой) до конца листа: в настоящем файле строк больше, чем видно.
        let whole = match (clicked, double) {
            (_, Hit::Cell(r, c)) => Some((c, Some(r))),
            (_, Hit::Column(c)) | (Hit::Column(c), _) => Some((c, None)),
            _ => None,
        };
        match clicked {
            _ if whole.is_some() => {}
            Hit::Cell(r, c) => {
                self.cursor = Some((c, r));
                self.sel = match self.drag_anchor {
                    Some((ac, a)) if shift && ac == c => Some((c, a.min(r), a.max(r))),
                    _ => {
                        self.drag_anchor = Some((c, r));
                        Some((c, r, r))
                    }
                };
            }
            _ => {}
        }
        if let Some((c, from)) = whole {
            let a = grid.column_start(from);
            self.sel = Some((c, a, nrows.max(a)));
            self.drag_anchor = Some((c, a));
            self.cursor = Some((c, a));
            self.new_to_end = true;
        }
        if let Hit::Cell(r, c) = drag_start {
            self.drag_anchor = Some((c, r));
            self.cursor = Some((c, r));
            self.sel = Some((c, r, r));
        }
        if let (Hit::Cell(r, _), Some((c, a))) = (drag_to, self.drag_anchor) {
            self.cursor = Some((c, r));
            self.sel = Some((c, a.min(r), a.max(r)));
        }
        if self.sel != old {
            if let Some((c, a, b)) = self.sel {
                self.range_text = format!("{0}{1}:{0}{2}", col_name(c), a, b);
            }
        }
        // Ячейка под правилом — правило справа подсвечивается и прокручивается в вид: его можно сразу править.
        if self.cursor != old_cursor {
            self.focus_rule = self.cursor.and_then(|(c, r)| self.docs[self.cur].profile.rules.iter().position(|x| x.sheet == sheet && x.covers(r, c)));
            self.scroll_rule = self.focus_rule.is_some();
        }
    }

    fn rules_panel(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.docs.get(self.cur) else {
            ui.heading("Правила замены");
            return;
        };
        ui.heading(format!("Правила замены ({})", doc.profile.rules.len()));
        if self.docs.len() > 1 {
            ui.label(RichText::new(format!("Файл: {}", file_name(&doc.path))).strong());
        }
        ui.label(RichText::new("Синим подсвечены ячейки под правилами. Формулы не меняются — они пересчитаются от фейков.").weak());
        ui.separator();
        let mut remove = None;
        let mut goto = None;
        let mut changed = false;
        let max_rows: HashMap<String, u32> = doc
            .wb
            .sheets
            .iter()
            .enumerate()
            .filter_map(|(i, s)| doc.grids.get(&i).map(|g| (s.name.clone(), g.max_row)))
            .collect();
        // Строк в листе на самом деле (загружены не все) — чтобы подписать, что предпросмотр по первым строкам.
        let sheet_rows: HashMap<String, u32> = doc
            .wb
            .sheets
            .iter()
            .enumerate()
            .filter_map(|(i, s)| doc.grids.get(&i).map(|g| (s.name.clone(), if g.truncated { g.dim_rows.unwrap_or(u32::MAX) } else { g.max_row })))
            .collect();
        // Названия колонок из шапки загруженных листов: (лист, колонка) → (строка шапки, текст).
        let heads: HashMap<(String, u32), (u32, String)> = doc
            .wb
            .sheets
            .iter()
            .enumerate()
            .filter_map(|(i, s)| doc.grids.get(&i).map(|g| (s.name.clone(), g)))
            .flat_map(|(name, g)| {
                let hr = g.header_row();
                (0..=g.max_col).filter_map(move |c| {
                    let hr = hr?;
                    let t = g.get(hr, c)?.text.trim().to_string();
                    (!t.is_empty()).then(|| ((name.clone(), c), (hr, t)))
                })
            })
            .collect();
        let cur_sel = self.sel.map(|s| (self.sheet_name(doc.cur_sheet), s));
        let sel_last = cur_sel.as_ref().and_then(|(sh, _)| max_rows.get(sh).copied());
        let preview_rules = self.preview_rules.get(self.cur);
        let counting = self.preview_job.is_some();
        let no_preview = Vec::new();
        let preview = self.preview.get(self.cur).unwrap_or(&no_preview);
        let mut take = None;
        let focus = self.focus_rule;
        let scroll = std::mem::take(&mut self.scroll_rule);
        // Правило, которому нужно подобрать подпись шаблона (сменили тип или диапазон).
        let mut refill = None;
        // Уже вписанные связи всех файлов — чтобы выбрать, а не набирать заново (и без опечаток).
        let mut used_links: Vec<String> =
            self.docs.iter().flat_map(|d| d.profile.rules.iter()).map(|r| r.link.trim().to_string()).filter(|l| !link_name(l).is_empty()).collect();
        used_links.sort_by_key(|l| link_name(l));
        used_links.dedup_by_key(|l| link_name(l));
        let doc = &mut self.docs[self.cur];
        egui::ScrollArea::vertical().id_salt(("rules", self.cur)).auto_shrink([false, false]).show(ui, |ui| {
            if doc.profile.rules.is_empty() {
                ui.label("Правил нет. Выделите диапазон в таблице и нажмите «Добавить правило» или используйте автоподбор.");
            }
            for (i, r) in doc.profile.rules.iter_mut().enumerate() {
                // Предпросмотр, посчитанный для этого же правила (правку ещё считают — не показываем старое).
                let pv = preview.get(i).filter(|_| preview_rules.and_then(|v| v.get(i)) == Some(&*r));
                let mut frame = egui::Frame::group(ui.style());
                if focus == Some(i) {
                    frame = frame.stroke(Stroke::new(2.0_f32, Color32::from_rgb(255, 165, 0)));
                }
                let shown = frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.strong(format!("{}!{}", r.sheet, r.range_text()));
                        if let Some(h) = heads.get(&(r.sheet.clone(), r.col)).filter(|(hr, _)| *hr < r.row_from) {
                            ui.label(format!("«{}»", h.1)).on_hover_text("Название колонки — из шапки таблицы");
                        }
                        match pv {
                            Some(p) => {
                                let part = sheet_rows.get(&r.sheet).is_some_and(|&n| n > GRID_ROWS);
                                let text = if part { format!("≈ {} ячеек в первых {GRID_ROWS} строках", p.cells) } else { format!("≈ {} ячеек", p.cells) };
                                ui.label(RichText::new(text).weak()).on_hover_text(
                                    "Предпросмотр считается по строкам, видным в окне. Точное число замен и проверка — в отчёте после обезличивания",
                                );
                            }
                            None if counting => {
                                ui.spinner();
                            }
                            None => {}
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Удалить").clicked() {
                                remove = Some(i);
                            }
                            if ui.small_button("Показать").clicked() {
                                goto = Some(i);
                            }
                            if ui
                                .add_enabled(cur_sel.is_some(), egui::Button::new("Взять выделение").small())
                                .on_hover_text("Перенести правило на диапазон, выделенный в таблице")
                                .clicked()
                            {
                                take = Some(i);
                            }
                        });
                    });
                    // С переносом: в узкой панели «Связь:» уходит на следующую строку, а не за край.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Строки с");
                        changed |= ui.add(egui::DragValue::new(&mut r.row_from).range(1..=1_048_576)).changed();
                        ui.label("по");
                        let from = r.row_from;
                        if let Some(to) = &mut r.row_to {
                            changed |= ui.add(egui::DragValue::new(to).range(from..=1_048_576)).changed();
                        }
                        let mut to_end = r.row_to.is_none();
                        if ui.checkbox(&mut to_end, "до конца").changed() {
                            r.row_to = if to_end { None } else { Some(max_rows.get(&r.sheet).copied().unwrap_or(from).max(from)) };
                            changed = true;
                        }
                        ui.label("колонка");
                        let mut col = col_name(r.col);
                        if ui.add(egui::TextEdit::singleline(&mut col).desired_width(36.0)).changed() {
                            if let Some(c) = finboard_anon::xlsx::parse_col(col.trim()) {
                                r.col = c;
                                changed = true;
                            }
                        }
                        // «Очистить» и «Не менять» фейков не дают — связь им не нужна.
                        if !matches!(r.kind, Kind::Clear | Kind::Keep) {
                            ui.label("Связь:").on_hover_text(LINK_HINT);
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut r.link).id(egui::Id::new(("link", i))).desired_width(80.0).hint_text("общая"))
                                .on_hover_text(LINK_HINT)
                                .changed();
                            let others: Vec<&String> = used_links.iter().filter(|l| link_name(l) != r.link_name()).collect();
                            if !others.is_empty() || !r.link.trim().is_empty() {
                                egui::ComboBox::from_id_salt(("link_pick", i)).selected_text("").width(14.0).show_ui(ui, |ui| {
                                    if !r.link.trim().is_empty() && ui.selectable_label(false, "общая (пусто)").clicked() {
                                        r.link.clear();
                                        changed = true;
                                    }
                                    for l in others {
                                        if ui.selectable_label(false, l.as_str()).clicked() {
                                            r.link = l.clone();
                                            changed = true;
                                        }
                                    }
                                });
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Тип правила:");
                        egui::ComboBox::from_id_salt(("kind", i)).selected_text(r.kind.title()).width(230.0).show_ui(ui, |ui| {
                            for k in Kind::ALL {
                                if ui.selectable_value(&mut r.kind, k, k.title()).on_hover_text(k.hint()).changed() {
                                    r.with_id = k.default_with_id();
                                    changed = true;
                                    refill = Some(i);
                                }
                            }
                        });
                        if r.kind.id_optional() {
                            changed |= ui.checkbox(&mut r.with_id, "с ID").on_hover_text("Добавлять ID для прослеживаемости").changed();
                        }
                        if r.kind == Kind::Text {
                            ui.label("подпись:");
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut r.label).desired_width(110.0).hint_text("из заголовка"))
                                .on_hover_text("Значения станут «подпись 1», «подпись 2»… — например «Налог 1», «Должность 2». У каждой подписи своя нумерация. Пусто — «Значение»")
                                .changed();
                        }
                    });
                    if let Some(p) = pv {
                        for (o, f) in &p.samples {
                            ui.label(RichText::new(format!("{o}  →  {f}")).small());
                        }
                        if p.below > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "Ниже строки {} в колонке ещё заполненных ячеек: {} — они не попадут под правило. Если таблица длиннее, отметьте «до конца»",
                                    r.row_to.unwrap_or(0),
                                    p.below
                                ))
                                .small()
                                .color(Color32::from_rgb(220, 80, 60)),
                            );
                        }
                    }
                    ui.label(RichText::new(format!("Пример: {}", r.kind.hint())).small().weak().italics());
                });
                if scroll && focus == Some(i) {
                    shown.response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
        if let Some(i) = remove {
            doc.profile.rules.remove(i);
            self.focus_rule = None;
            changed = true;
        }
        if let (Some(i), Some((sheet, (c, a, b)))) = (take, cur_sel) {
            let r = &mut doc.profile.rules[i];
            r.sheet = sheet;
            r.col = c;
            r.row_from = a;
            r.row_to = if self.new_to_end || sel_last.is_some_and(|l| b >= l) { None } else { Some(b) };
            // Новый диапазон — может быть, другая колонка: подпись берётся из её заголовка.
            if r.kind == Kind::Text {
                r.label.clear();
            }
            changed = true;
            refill = Some(i);
        }
        if let Some(i) = goto {
            let r = doc.profile.rules[i].clone();
            if let Some(idx) = doc.wb.sheet_index(&r.sheet) {
                doc.cur_sheet = idx;
                self.ensure_grid(idx);
                let to = r.row_to.unwrap_or_else(|| self.docs[self.cur].grids.get(&idx).map_or(r.row_from, |g| g.max_row));
                self.sel = Some((r.col, r.row_from, to.max(r.row_from)));
                self.drag_anchor = Some((r.col, r.row_from));
                self.scroll_to = Some((r.row_from, r.col));
                self.range_text = r.range_text().replace('…', "");
                self.cursor = Some((r.col, r.row_from));
                self.focus_rule = Some(i);
            }
        }
        if let Some(i) = refill {
            let mut r = self.docs[self.cur].profile.rules[i].clone();
            if let Some(n) = self.fill_template(&mut r) {
                self.docs[self.cur].profile.rules[i] = r;
                self.set_status(n, false);
            }
        }
        if changed {
            self.preview_dirty = true;
        }
    }

    fn bottom_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let s = &mut self.settings;
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Формат ID:");
            changed |= ui
                .add(egui::TextEdit::singleline(&mut s.id_format).desired_width(150.0))
                .on_hover_text("Для ФИО, адреса, организации. {fake} — фейк, {id} — номер")
                .changed();
            ui.label("Домен email:");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.email_domain).desired_width(120.0)).changed();
            ui.checkbox(&mut s.clean_metadata, "Убрать автора, компанию, имена пользователей и пути их папок из служебных полей файла");
            ui.checkbox(&mut s.recalc_on_open, "Пересчёт формул при открытии в Excel");
            // Профиль прежних версий: снятая галочка «искать внутри текста» = «только целые ячейки».
            if !s.inner_text && s.outside == Outside::All {
                s.outside = Outside::Cells;
            }
            ui.label("Вне разметки:");
            egui::ComboBox::from_id_salt("outside").selected_text(s.outside.title()).width(170.0).show_ui(ui, |ui| {
                for o in Outside::ALL {
                    changed |= ui.selectable_value(&mut s.outside, o, o.title()).on_hover_text(o.hint()).changed();
                }
            })
            .response
            .on_hover_text("Что делать с теми же значениями (ФИО, организации, ИНН, телефоны) в ячейках без правил — на других листах и в других файлах.\nОтдельные области можно защитить правилом «Не менять (защитить)»");
            s.inner_text = s.outside == Outside::All;
        });
        if changed {
            self.preview_dirty = true;
        }
        ui.horizontal(|ui| {
            ui.label("Ключ:");
            let k = match (&self.key_in, self.docs.first()) {
                (Some(p), _) => p.display().to_string(),
                (None, Some(d)) => format!("новый — будет записан в {}", key_out_path(&d.path, None, false).display()),
                (None, None) => "не выбран".into(),
            };
            ui.label(RichText::new(k).weak()).on_hover_text(
                "Ключ общий для всех файлов папки: те же люди, организации и ИНН получают те же фейки и ID в каждом файле и при каждом запуске",
            );
            if ui.button("Выбрать…").on_hover_text("Ключ прошлых запусков — например, из другой папки того же заказчика. Новые соответствия допишутся в него").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Ключ CSV", &["csv"]).pick_file() {
                    self.key_in = Some(p);
                    self.key_chosen = true;
                    self.preview_dirty = true;
                }
            }
            if self.key_in.is_some()
                && ui.button("Не использовать").on_hover_text("Новые фейки и ID с 1. Прежний ключ не затирается — сохранится с пометкой «прежний»").clicked()
            {
                self.key_in = None;
                self.key_chosen = false;
                self.preview_dirty = true;
            }
            ui.separator();
            ui.checkbox(&mut self.save_key, "Сохранить ключ ID ↔ реальные данные (не передавать)");
            ui.checkbox(&mut self.save_profile, "Сохранять профиль рядом с файлом")
                .on_hover_text("<имя>_профиль.json — при следующем открытии этого файла правила загрузятся сами");
        });
        ui.horizontal(|ui| {
            let running = self.job.is_some();
            let on = self.docs.iter().filter(|d| d.on).count();
            let can = on > 0 && !running && self.opening.is_none();
            let label = match (self.docs.len(), on) {
                (n, k) if n > 1 && k == n => format!("Обезличить все файлы ({n})…"),
                (n, k) if n > 1 => format!("Обезличить отмеченные файлы ({k} из {n})…"),
                _ => "Обезличить и сохранить…".into(),
            };
            if ui.add_enabled(can, egui::Button::new(RichText::new(label).strong().size(16.0))).clicked() {
                self.start_job(ui.ctx());
            }
            if running {
                ui.spinner();
            }
        });
        if let (true, Some((p, t0))) = (self.job.is_some(), &self.progress) {
            let f = p.fraction();
            let text = progress_label(f, &self.stage, &self.file_line, t0.elapsed());
            ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width()).text(text));
        }
        ui.separator();
        egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in &self.log {
                let mut t = RichText::new(l).monospace();
                if l.starts_with("Ошибка") || l.starts_with("Проверка обезличенного файла: в нём ещё") || l.starts_with("Внимание") {
                    t = t.color(Color32::from_rgb(220, 80, 60));
                }
                ui.label(t);
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}

impl App {
    /// Вопрос при закрытии окна во время обработки.
    fn close_dialog(&mut self, ctx: &egui::Context) {
        let mut answer = None;
        egui::Window::new("Закрыть окно?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(RichText::new("Идёт обезличивание. Прервать?").strong());
                ui.label("Записанное до сих пор будет удалено; прежние результаты и ключ останутся как были.");
                ui.horizontal(|ui| {
                    if ui.button("Прервать и закрыть").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Продолжить обработку").clicked() {
                        answer = Some(false);
                    }
                });
            });
        match answer {
            Some(true) => self.confirm_close(),
            Some(false) => self.close_ask = false,
            None => {}
        }
    }

    /// «Прервать и закрыть»: поток обработки останавливается на ближайшей части книги и удаляет временные
    /// файлы; окно закрывается, когда он закончит (см. `ui`).
    fn confirm_close(&mut self) {
        self.close_ask = false;
        self.closing = true;
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed);
        }
        self.set_status("Прерывание… окно закроется, когда временные файлы будут удалены", false);
    }

    /// Кадр окна — без eframe, чтобы тесты гоняли его на настоящих событиях мыши и клавиатуры.
    fn ui(&mut self, ctx: &egui::Context) {
        self.poll_job();
        self.poll_open();
        // Прервано закрытием окна — поток убрал временные файлы и закончил: теперь окно закрывается.
        if self.closing && self.job.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Закрытие окна во время обработки — только с вопросом: иначе поток обрывается на полуслове,
        // а рядом с результатами остаются временные файлы с данными.
        // (Сведения об окне — через `get`: в тестах окна без экрана их нет, `viewport()` упал бы.)
        let close_requested = ctx.input(|i| i.raw.viewports.get(&i.raw.viewport_id).is_some_and(|v| v.close_requested()));
        if self.job.is_some() && close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing {
                self.close_ask = true;
            }
        }
        // Полоса хода и время обновляются и без сообщений из потока обработки.
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        // Перетащенная папка — все файлы Excel из неё (обход — в фоне).
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect::<Vec<_>>())
            .into_iter()
            .filter(|p| p.is_dir() || is_excel(p))
            .collect();
        if !dropped.is_empty() {
            self.open_files_bg(dropped, false, ctx);
        }
        // Обработка закончилась, пока висел вопрос, — спрашивать больше не о чем.
        if self.close_ask && self.job.is_some() {
            self.close_dialog(ctx);
        } else {
            self.close_ask = false;
        }
        egui::TopBottomPanel::top("top").show(ctx, |ui| self.top_bar(ui));
        egui::TopBottomPanel::bottom("bottom").resizable(true).default_height(230.0).show(ctx, |ui| self.bottom_panel(ui));
        egui::SidePanel::left("sheets").resizable(true).default_width(210.0).show(ctx, |ui| self.sheets_panel(ui));
        egui::SidePanel::right("rules").resizable(true).min_width(500.0).default_width(580.0).show(ctx, |ui| self.rules_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.grid_panel(ui));
        // Не пересчитываем, пока кнопка мыши зажата (тянут поле со строками) — файлы читаются целиком.
        // Правка во время подсчёта — пересчёт сразу после него.
        self.poll_preview();
        if self.preview_dirty && self.preview_job.is_none() && !ctx.input(|i| i.pointer.any_down()) {
            self.start_preview(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, PointerButton};

    /// Окно без экрана: кадры с событиями мыши и клавиатуры, как от пользователя.
    struct Screen {
        ctx: egui::Context,
        t: f64,
    }

    impl Screen {
        fn new() -> Screen {
            Screen { ctx: egui::Context::default(), t: 0.0 }
        }

        fn frame(&mut self, app: &mut App, events: Vec<Event>) {
            self.t += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1500.0, 920.0))),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let _ = self.ctx.run(input, |ctx| app.ui(ctx));
        }

        fn click(&mut self, app: &mut App, p: egui::Pos2) {
            let button = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            self.frame(app, vec![Event::PointerMoved(p)]);
            self.frame(app, vec![button(true)]);
            self.frame(app, vec![button(false)]);
            // Пауза больше двойного щелчка: следующий щелчок — новый.
            self.t += 1.0;
            self.frame(app, vec![]);
        }

        fn key(&mut self, app: &mut App, key: Key, modifiers: Modifiers) {
            let ev = |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            self.frame(app, vec![ev(true)]);
            self.frame(app, vec![ev(false)]);
            self.frame(app, vec![]);
        }

        /// Середина ячейки на экране — по прямоугольнику таблицы в прошлом кадре.
        fn cell(&self, app: &App, c: u32, r: u32) -> egui::Pos2 {
            let rect = self.ctx.read_response(app.grid_id.expect("таблица не нарисована")).unwrap().rect;
            let d = &app.docs[app.cur];
            let w = &d.widths[&d.cur_sheet];
            let x: f32 = w[..c as usize].iter().sum();
            rect.min + vec2(ROWNUM_W + x + w[c as usize] / 2.0, HEAD_H + (r - 1) as f32 * ROW_H + ROW_H / 2.0)
        }
    }

    fn sample() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples").join("demo_dashboard.xlsx")
    }

    fn sample_app() -> (App, Screen) {
        let mut app = App::blank();
        app.open_files(vec![sample()], true);
        assert_eq!(app.docs.len(), 1, "{}", app.status);
        let mut s = Screen::new();
        s.frame(&mut app, vec![]);
        s.frame(&mut app, vec![]);
        (app, s)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fb_main_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Стрелки после щелчка по ячейке двигали курсор только изредка: фокус таблицы сам же их и отключал.
    /// Каждое нажатие — подряд, в отдельных кадрах, как у человека.
    #[test]
    fn arrows_move_cursor_after_click() {
        let (mut app, mut s) = sample_app();
        let p = s.cell(&app, 1, 3);
        s.click(&mut app, p);
        assert_eq!(app.cursor, Some((1, 3)), "щелчок ставит курсор");
        for (key, want) in [
            (Key::ArrowRight, (2, 3)),
            (Key::ArrowRight, (3, 3)),
            (Key::ArrowDown, (3, 4)),
            (Key::ArrowDown, (3, 5)),
            (Key::ArrowLeft, (2, 5)),
            (Key::ArrowUp, (2, 4)),
            (Key::ArrowLeft, (1, 4)),
            (Key::ArrowLeft, (0, 4)),
            (Key::ArrowLeft, (0, 4)),
            (Key::ArrowUp, (0, 3)),
        ] {
            s.key(&mut app, key, Modifiers::NONE);
            assert_eq!(app.cursor, Some(want), "{key:?}");
        }
        assert_eq!(app.sel, Some((0, 3, 3)), "выделение идёт за курсором");
        s.key(&mut app, Key::ArrowDown, Modifiers::SHIFT);
        s.key(&mut app, Key::ArrowDown, Modifiers::SHIFT);
        assert_eq!(app.sel, Some((0, 3, 5)), "Shift+стрелка расширяет выделение");
        assert_eq!(app.range_text, "A3:A5");
        s.key(&mut app, Key::Home, Modifiers::COMMAND);
        assert_eq!(app.cursor, Some((0, 1)));
    }

    /// Пока печатают диапазон в поле, стрелки двигают текстовый курсор, а не таблицу.
    #[test]
    fn arrows_leave_text_field_alone() {
        let (mut app, mut s) = sample_app();
        let p = s.cell(&app, 2, 2);
        s.click(&mut app, p);
        s.ctx.memory_mut(|m| m.request_focus(egui::Id::new("range_text")));
        s.frame(&mut app, vec![]);
        s.key(&mut app, Key::ArrowRight, Modifiers::NONE);
        s.key(&mut app, Key::ArrowDown, Modifiers::NONE);
        assert_eq!(app.cursor, Some((2, 2)));
        // Вписанный руками диапазон не затирается выделением в таблице.
        app.range_text = "C5:C120".into();
        s.frame(&mut app, vec![]);
        assert_eq!(app.range_text, "C5:C120");
        // Открытый выпадающий список (тип правила) — стрелки выбирают в нём.
        let p = s.cell(&app, 2, 2);
        s.click(&mut app, p);
        s.ctx.memory_mut(|m| m.open_popup(egui::Id::new("new_kind")));
        s.key(&mut app, Key::ArrowDown, Modifiers::NONE);
        assert_eq!(app.cursor, Some((2, 2)), "стрелка ушла в таблицу при открытом списке");
        s.ctx.memory_mut(|m| m.close_popup());
        // Снова щелчок по таблице — стрелки опять её.
        let p = s.cell(&app, 2, 2);
        s.click(&mut app, p);
        s.key(&mut app, Key::ArrowDown, Modifiers::NONE);
        assert_eq!(app.cursor, Some((2, 3)));
    }

    /// Ячейка под правилом — правило справа подсвечено (его номер в списке); вне правил — подсветки нет.
    #[test]
    fn cell_under_rule_points_to_rule() {
        let (mut app, mut s) = sample_app();
        let sheet = app.sheet_name(0);
        app.docs[0].profile.rules.push(Rule::new(&sheet, 0, 2, Some(3), Kind::FullName));
        app.docs[0].profile.rules.push(Rule::new(&sheet, 1, 2, None, Kind::FullName));
        s.frame(&mut app, vec![]);
        let p = s.cell(&app, 1, 4);
        s.click(&mut app, p);
        assert_eq!(app.focus_rule, Some(1));
        s.key(&mut app, Key::ArrowLeft, Modifiers::NONE);
        assert_eq!(app.focus_rule, None, "A4 ниже правила A2:A3");
        s.key(&mut app, Key::ArrowUp, Modifiers::NONE);
        assert_eq!(app.focus_rule, Some(0));
        s.key(&mut app, Key::ArrowRight, Modifiers::NONE);
        s.key(&mut app, Key::ArrowRight, Modifiers::NONE);
        assert_eq!(app.focus_rule, None);
    }

    /// Папка: файлы Excel из всех подпапок; результаты прошлых запусков (рядом с исходником) и временные файлы Excel — нет.
    #[test]
    fn folder_lists_excel_files_recursively() {
        let d = temp_dir("folder");
        std::fs::create_dir_all(d.join("2024").join("март")).unwrap();
        for f in [
            "Продажи.xlsx",
            "Продажи_anon.xlsx",
            "Продажи_anon_2.xlsx",
            "~$Продажи.xlsx",
            "Отчёт.XLSM",
            "заметки.txt",
            // Обезличен заранее, исходника рядом нет — это исходник для программы (тестовые файлы клиента).
            "Продажи2019-2026_anon.xlsx",
            "Отчёт_anon_итог.xlsx",
            "2024/Затраты.xlsx",
            "2024/Затраты_anon.xlsx",
            "2024/март/ДДС.xlsx",
            "2024/ДДС_anon.xlsx",
        ] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let names: Vec<String> = excel_files_in(&d).iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(
            names,
            ["2024/ДДС_anon.xlsx", "2024/Затраты.xlsx", "2024/март/ДДС.xlsx", "Отчёт.XLSM", "Отчёт_anon_итог.xlsx", "Продажи.xlsx", "Продажи2019-2026_anon.xlsx"]
        );
    }

    /// Файл без галочки не обрабатывается и в предпросмотр не даёт значений.
    #[test]
    fn unchecked_file_is_left_out() {
        let d = temp_dir("checked");
        let (a, b) = (d.join("a.xlsx"), d.join("b.xlsx"));
        std::fs::copy(sample(), &a).unwrap();
        std::fs::copy(sample(), &b).unwrap();
        let mut app = App::blank();
        app.open_files(vec![a, b], true);
        let sheet = app.sheet_name(0);
        for doc in &mut app.docs {
            doc.profile.rules.push(Rule::new(&sheet, 0, 2, None, Kind::FullName));
        }
        app.docs[1].on = false;
        app.start_preview(&egui::Context::default());
        let (rules, res) = app.preview_job.take().unwrap().recv().unwrap();
        assert_eq!(rules[0].len(), 1);
        assert!(rules[1].is_empty(), "неотмеченный файл — без правил");
        assert!(res.is_ok(), "{res:?}");
    }

    /// Время по-русски: секунды, минуты с секундами в две цифры, часы с минутами; округление до секунды.
    #[test]
    fn duration_in_russian() {
        let s = |x: f64| format_duration(Duration::from_secs_f64(x));
        assert_eq!(s(0.0), "0 с");
        assert_eq!(s(45.0), "45 с");
        assert_eq!(s(59.4), "59 с");
        assert_eq!(s(59.6), "1 мин 00 с");
        assert_eq!(s(60.0), "1 мин 00 с");
        assert_eq!(s(125.0), "2 мин 05 с");
        assert_eq!(s(190.0), "3 мин 10 с");
        assert_eq!(s(3599.0), "59 мин 59 с");
        assert_eq!(s(3600.0), "1 ч 00 мин");
        assert_eq!(s(3720.0), "1 ч 02 мин");
        assert_eq!(s(7385.0), "2 ч 03 мин");
    }

    /// Надпись полосы хода: проценты вниз, файл и этап, остаток — только когда доля заметна, в конце — затраченное время.
    #[test]
    fn progress_label_parts_and_eta() {
        let secs = Duration::from_secs;
        // Начало: остаток ещё не оценивается.
        assert_eq!(progress_label(0.0, "", "", secs(0)), "0 % · осталось: оценка…");
        assert_eq!(progress_label(0.0, "сбор значений", "", secs(3)), "0 % — сбор значений · осталось: оценка…");
        // Малая доля (2 %) — оценка по ней была бы случайной.
        assert_eq!(progress_label(0.02, "сбор значений", "", secs(10)), "2 % — сбор значений · осталось: оценка…");
        // Чуть больше 3 % — уже оценивается: 31 с × 0,969 / 0,031 = 969 с.
        assert_eq!(progress_label(0.031, "", "", secs(31)), "3 % · осталось ≈ 16 мин 09 с");
        // Середина: 48 с за 37,5 % → осталось 48 × 0,625 / 0,375 = 80 с.
        assert_eq!(
            progress_label(0.375, "запись", "Файл 2 из 10: Продажи.xlsx", secs(48)),
            "37 % — Файл 2 из 10: Продажи.xlsx — запись · осталось ≈ 1 мин 20 с"
        );
        // Четверть за минуту — осталось три минуты (не 20 с и не 4 мин).
        assert_eq!(progress_label(0.25, "запись", "", secs(60)), "25 % — запись · осталось ≈ 3 мин 00 с");
        // Строка файла без этапа (этап сброшен новым файлом).
        assert_eq!(progress_label(0.5, "", "Файл 3 из 10: Б.xlsx", secs(10)), "50 % — Файл 3 из 10: Б.xlsx · осталось ≈ 10 с");
        // 99,9 % — ещё не 100.
        assert_eq!(progress_label(0.999, "проверка результата", "", secs(999)), "99 % — проверка результата · осталось ≈ 1 с");
        // Готово: без остатка, с затраченным временем.
        assert_eq!(progress_label(1.0, "проверка результата", "", secs(125)), "100 % — проверка результата · за 2 мин 05 с");
    }

    /// Этапы из журнала: «Файл k из n» запоминается и сбрасывает этап; конец — время в строке состояния, ход убран.
    #[test]
    fn job_stages_and_time_in_status() {
        assert_eq!(stage_of("Сбор значений по правилам…"), Some("сбор значений"));
        assert_eq!(stage_of("Запись файла…"), Some("запись"));
        assert_eq!(stage_of("Проверка результата на остатки реальных значений…"), Some("проверка результата"));
        assert_eq!(stage_of("Уникальных значений: 5"), None);
        assert_eq!(stage_of("Проверка: реальных значений из правил в файле не найдено."), None);

        let mut app = App::blank();
        let (tx, rx) = channel();
        app.job = Some(rx);
        app.progress = Some((Arc::new(Progress::default()), Instant::now()));
        let mut s = Screen::new();
        let log = |m: &str| Msg::Log(m.into());
        tx.send(log("Сбор значений по правилам…")).unwrap();
        tx.send(log("Уникальных значений: 5")).unwrap();
        s.frame(&mut app, vec![]);
        assert_eq!((app.stage.as_str(), app.file_line.as_str()), ("сбор значений", ""));
        tx.send(log("Ключ записан: D:\\к.csv")).unwrap();
        tx.send(log("Файл 2 из 10: Продажи.xlsx")).unwrap();
        s.frame(&mut app, vec![]);
        assert_eq!((app.stage.as_str(), app.file_line.as_str()), ("", "Файл 2 из 10: Продажи.xlsx"));
        tx.send(log("Запись файла…")).unwrap();
        s.frame(&mut app, vec![]);
        assert_eq!((app.stage.as_str(), app.file_line.as_str()), ("запись", "Файл 2 из 10: Продажи.xlsx"));
        assert_eq!(app.log.len(), 5, "строки этапов остаются и в журнале");
        assert!(app.job.is_some() && app.progress.is_some());

        let rep = Report { output: PathBuf::from("D:\\р_anon.xlsx"), ..Default::default() };
        tx.send(Msg::Done(Ok(vec![rep]), vec![Profile::default()])).unwrap();
        s.frame(&mut app, vec![]);
        assert!(app.job.is_none() && app.progress.is_none());
        assert_eq!(app.status, "Готово: D:\\р_anon.xlsx — за 0 с");
        assert!(!app.status_err);
    }

    /// Правило с другого листа не подсвечивается у ячейки этого листа, хотя колонка и строки совпадают.
    #[test]
    fn rule_on_other_sheet_is_not_pointed_to() {
        let (mut app, mut s) = sample_app();
        assert!(app.docs[0].wb.sheets.len() > 1);
        let other = app.sheet_name(1);
        app.docs[0].profile.rules.push(Rule::new(&other, 1, 1, None, Kind::FullName));
        s.frame(&mut app, vec![]);
        let p = s.cell(&app, 1, 4);
        s.click(&mut app, p);
        assert_eq!(app.cursor, Some((1, 4)));
        assert_eq!(app.focus_rule, None, "подсвечено правило листа «{other}»");
    }

    /// «Нет правил в файлах: …» называет только отмеченные файлы: файл без галочки не обрабатывается,
    /// правила ему не нужны.
    #[test]
    fn no_rules_message_names_only_checked_files() {
        let d = temp_dir("norules");
        let (a, b) = (d.join("a.xlsx"), d.join("b.xlsx"));
        std::fs::copy(sample(), &a).unwrap();
        std::fs::copy(sample(), &b).unwrap();
        let mut app = App::blank();
        app.open_files(vec![a, b], true);
        app.docs[1].on = false;
        app.start_job(&egui::Context::default());
        assert!(app.job.is_none());
        assert_eq!(app.status, "Нет правил в файлах: a.xlsx — добавьте правила или снимите с них галочку");
        assert!(app.status_err);
    }

    /// Похожее на результат, но не результат программы («_anon_» без номера, «_anon_2024г») — это исходник:
    /// он в списке, даже если рядом лежит файл с тем же началом имени.
    #[test]
    fn folder_keeps_files_that_only_look_like_results() {
        let d = temp_dir("lookalike");
        for f in ["Продажи.xlsx", "Продажи_anon_.xlsx", "Продажи_anon_2024г.xlsx", "Продажи_anon_7.xlsx"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let names: Vec<String> = excel_files_in(&d).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Продажи.xlsx", "Продажи_anon_.xlsx", "Продажи_anon_2024г.xlsx"]);
    }

    /// Поток обработки оборвался без итога: окно не должно навсегда остаться в «Обработка…».
    #[test]
    fn job_thread_gone_without_result_shows_error() {
        let mut app = App::blank();
        let (tx, rx) = channel();
        app.job = Some(rx);
        app.progress = Some((Arc::new(Progress::default()), Instant::now()));
        app.set_status("Обработка…", false);
        let mut s = Screen::new();
        tx.send(Msg::Log("Запись файла…".into())).unwrap();
        s.frame(&mut app, vec![]);
        assert!(app.job.is_some(), "поток ещё жив — ждём");
        drop(tx);
        s.frame(&mut app, vec![]);
        assert!(app.job.is_none() && app.progress.is_none(), "обработка не сброшена");
        assert!(app.status_err, "{}", app.status);
        let last = app.log.last().unwrap();
        assert_eq!(last, &format!("Ошибка: {}", crash_message(None)));
        assert!(last.contains("Исходные файлы не изменены"), "{last}");
        assert_eq!(app.log.len(), 2, "строки журнала до сбоя сохраняются: {:?}", app.log);
    }

    /// Паника в потоке обработки — та же ошибка, с текстом паники; итог, пришедший до обрыва канала, не теряется.
    #[test]
    fn job_panic_becomes_error_with_text() {
        let ok: Result<u8, String> = guarded(|| Ok(7));
        assert_eq!(ok, Ok(7));
        assert_eq!(guarded::<u8>(|| Err("плохо".into())), Err("плохо".into()));
        let e = guarded::<u8>(|| panic!("индекс за пределами")).unwrap_err();
        assert!(e.starts_with("сбой обработки") && e.contains("(причина: индекс за пределами)"), "{e}");
        let e = guarded::<u8>(|| panic!("{} строк", 5)).unwrap_err();
        assert!(e.contains("(причина: 5 строк)"), "{e}");

        let mut app = App::blank();
        let (tx, rx) = channel();
        app.job = Some(rx);
        app.progress = Some((Arc::new(Progress::default()), Instant::now()));
        let worker = std::thread::spawn(move || {
            let res = guarded(|| -> Result<Vec<Report>, String> { panic!("сломалось") });
            let _ = tx.send(Msg::Done(res, Vec::new()));
        });
        worker.join().unwrap();
        let mut s = Screen::new();
        s.frame(&mut app, vec![]);
        assert!(app.job.is_none() && app.progress.is_none());
        assert!(app.status_err);
        assert!(app.status.contains("(причина: сломалось)"), "{}", app.status);
        assert_eq!(app.log.iter().filter(|l| l.contains("сбой обработки")).count(), 1, "{:?}", app.log);
    }

    /// «Связь:» на карточке правила: напечатанное слово попадает в правило, а с ним — в ключ типа.
    /// У «Очистить» поля нет: фейков у этого типа нет, связь ему ни к чему.
    #[test]
    fn typing_link_updates_rule() {
        let (mut app, mut s) = sample_app();
        let sheet = app.sheet_name(0);
        app.docs[0].profile.rules.push(Rule::new(&sheet, 1, 2, None, Kind::Address));
        app.docs[0].profile.rules.push(Rule::new(&sheet, 2, 2, None, Kind::Clear));
        s.frame(&mut app, vec![]);
        let field = egui::Id::new(("link", 0usize));
        let rect = s.ctx.read_response(field).expect("поля «Связь:» нет на карточке").rect;
        assert!(rect.max.x <= 1500.0 && rect.min.x > 0.0, "поле «Связь:» за краем окна: {rect:?}");
        assert!(s.ctx.read_response(egui::Id::new(("link", 1usize))).is_none(), "у «Очистить» поле связи не нужно");
        s.ctx.memory_mut(|m| m.request_focus(field));
        s.frame(&mut app, vec![]);
        s.frame(&mut app, vec![Event::Text("Контрагент".into())]);
        s.frame(&mut app, vec![]);
        let r = &app.docs[0].profile.rules[0];
        assert_eq!(r.link, "Контрагент");
        assert_eq!(r.kind_key(), "Адрес+ID@контрагент");
        assert_eq!(app.docs[0].profile.rules[1].link, "", "печать ушла не в то правило");
    }

    /// Кадр с запросом закрытия окна (крестик) или без; возвращает команды окну, отданные в этом кадре.
    fn frame_close(s: &mut Screen, app: &mut App, close: bool) -> Vec<egui::ViewportCommand> {
        s.t += 0.05;
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1500.0, 920.0))),
            time: Some(s.t),
            ..Default::default()
        };
        let vp = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        if close {
            vp.events.push(egui::ViewportEvent::Close);
        }
        let out = s.ctx.run(input, |ctx| app.ui(ctx));
        out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
    }

    /// Закрытие окна во время обработки обрывало поток без вопроса, рядом с результатами оставались временные
    /// файлы. Теперь закрытие отменяется и звучит вопрос «Идёт обезличивание. Прервать?»; «Прервать» поднимает
    /// флаг отмены (поток удаляет временные файлы), окно закрывается само, когда поток закончил.
    #[test]
    fn closing_window_during_job_asks_and_cancels() {
        use egui::ViewportCommand::{CancelClose, Close};
        let mut app = App::blank();
        let mut s = Screen::new();
        // Без обработки окно закрывается как обычно.
        let cmds = frame_close(&mut s, &mut app, true);
        assert!(!cmds.contains(&CancelClose) && !app.close_ask, "{cmds:?}");

        let (tx, rx) = channel();
        app.job = Some(rx);
        app.progress = Some((Arc::new(Progress::default()), Instant::now()));
        let flag = Arc::new(AtomicBool::new(false));
        app.cancel = Some(flag.clone());
        let cmds = frame_close(&mut s, &mut app, true);
        assert!(cmds.contains(&CancelClose), "окно закрылось посреди обработки: {cmds:?}");
        assert!(app.close_ask, "нет вопроса");
        assert!(!flag.load(Ordering::Relaxed), "отмена без ответа");
        // «Продолжить обработку» — вопрос убран, обработка идёт.
        app.close_ask = false;
        frame_close(&mut s, &mut app, false);
        assert!(!app.close_ask && !flag.load(Ordering::Relaxed));
        // Снова крестик и «Прервать и закрыть».
        frame_close(&mut s, &mut app, true);
        assert!(app.close_ask);
        app.confirm_close();
        assert!(flag.load(Ordering::Relaxed), "флаг отмены не поднят");
        assert!(app.status.starts_with("Прерывание"), "{}", app.status);
        // Поток ещё убирает за собой: крестик снова отменяется, вопрос не повторяется, окно не закрывается.
        let cmds = frame_close(&mut s, &mut app, true);
        assert!(cmds.contains(&CancelClose) && !cmds.contains(&Close) && !app.close_ask, "{cmds:?}");
        // Поток закончил — окно закрывается.
        tx.send(Msg::Done(Err("Обезличивание прервано.".into()), Vec::new())).unwrap();
        let cmds = frame_close(&mut s, &mut app, false);
        assert!(cmds.contains(&Close), "окно не закрылось после отмены: {cmds:?}");
        assert!(app.job.is_none() && app.cancel.is_none());
    }

    /// Папка открывается в фоне: окно не ждёт ни обхода папки, ни чтения файлов (на больших папках замирало);
    /// ход — в строке состояния, по окончании файлы в списке. Пустая папка — сообщение.
    #[test]
    fn folder_opens_in_background() {
        let d = temp_dir("bg_open");
        for n in ["a", "b", "c"] {
            std::fs::copy(sample(), d.join(format!("{n}.xlsx"))).unwrap();
        }
        let mut app = App::blank();
        let mut s = Screen::new();
        let wait = |app: &mut App, s: &mut Screen| {
            let t0 = Instant::now();
            while app.opening.is_some() {
                assert!(t0.elapsed() < Duration::from_secs(60), "открытие не закончилось: {}", app.status);
                s.frame(app, vec![]);
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        app.open_files_bg(vec![d.clone()], false, &s.ctx);
        assert!(app.opening.is_some(), "открытие не в фоне");
        assert!(app.docs.is_empty(), "файлы прочитаны в потоке окна");
        wait(&mut app, &mut s);
        assert_eq!(app.docs.len(), 3, "{}", app.status);
        assert_eq!(app.status, "Открыто файлов: 3");
        // Повторное открытие той же папки — те же три файла, без повторов.
        app.open_files_bg(vec![d.clone()], false, &s.ctx);
        wait(&mut app, &mut s);
        assert_eq!(app.docs.len(), 3);
        let e = temp_dir("bg_empty");
        app.open_files_bg(vec![e.clone()], false, &s.ctx);
        wait(&mut app, &mut s);
        assert!(app.status_err && app.status.contains("нет файлов Excel"), "{}", app.status);
        assert_eq!(app.docs.len(), 3);
    }

    /// Точка соединения (junction) на саму папку зацикливала обход («петля\петля\…» до предела длины пути,
    /// с повторами файлов), на чужую папку — подмешивала чужие файлы. Ссылки теперь не открываются.
    #[cfg(windows)]
    #[test]
    fn folder_skips_junctions() {
        let d = temp_dir("junction");
        let other = temp_dir("junction_other");
        std::fs::write(d.join("a.xlsx"), b"x").unwrap();
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub").join("b.xlsx"), b"x").unwrap();
        std::fs::write(other.join("other.xlsx"), b"x").unwrap();
        for (link, target) in [(d.join("loop"), &d), (d.join("sub").join("up"), &d), (d.join("foreign"), &other)] {
            let st = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(target).output().unwrap();
            assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        }
        let names: Vec<String> = excel_files_in(&d).iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(names, ["a.xlsx", "sub/b.xlsx"]);
    }

    /// Ссылка, названная как книга («архив.xlsx» — точка соединения на чужую папку; так же выглядит
    /// символическая ссылка на файл): не папка и не книга этой папки — в список не попадает.
    #[cfg(windows)]
    #[test]
    fn folder_skips_link_named_like_book() {
        let d = temp_dir("junction_named");
        let other = temp_dir("junction_named_other");
        std::fs::write(d.join("a.xlsx"), b"x").unwrap();
        std::fs::write(other.join("чужой.xlsx"), b"x").unwrap();
        let st = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(d.join("архив.xlsx")).arg(&other).output().unwrap();
        assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
        let names: Vec<String> = excel_files_in(&d).iter().map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.xlsx"]);
    }

    /// Вторая папка, брошенная в окно, пока первая ещё открывается: просьба подождать, а файлы первой — в списке.
    /// Без проверки второе открытие подменяло первое, и файлы первой папки молча терялись.
    #[test]
    fn second_open_while_opening_is_refused() {
        let a = temp_dir("bg_first");
        let b = temp_dir("bg_second");
        for n in ["a", "b", "c"] {
            std::fs::copy(sample(), a.join(format!("{n}.xlsx"))).unwrap();
        }
        std::fs::copy(sample(), b.join("z.xlsx")).unwrap();
        let mut app = App::blank();
        let mut s = Screen::new();
        app.open_files_bg(vec![a.clone()], false, &s.ctx);
        app.open_files_bg(vec![b.clone()], false, &s.ctx);
        assert!(app.status_err && app.status.starts_with("Дождитесь"), "{}", app.status);
        let t0 = Instant::now();
        while app.opening.is_some() {
            assert!(t0.elapsed() < Duration::from_secs(60), "открытие не закончилось: {}", app.status);
            s.frame(&mut app, vec![]);
            std::thread::sleep(Duration::from_millis(10));
        }
        let names: Vec<String> = app.docs.iter().map(|d| file_name(&d.path)).collect();
        assert_eq!(names, ["a.xlsx", "b.xlsx", "c.xlsx"], "{}", app.status);
    }
}
