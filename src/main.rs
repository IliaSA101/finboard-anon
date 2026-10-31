#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, RichText, Sense, Stroke};

use finboard_anon::anon::{
    batch_outputs, default_output_path, find_key, key_out_path, preview_batch, run_batch, Batch, FileJob, Report,
    RulePreview,
};
use finboard_anon::detect;
use finboard_anon::profile::{Kind, Outside, Profile, Rule, Settings};
use finboard_anon::xlsx::{col_name, load_grid, parse_range, Grid, GridCell, Workbook};

/// Сколько строк листа загружать для просмотра.
const GRID_ROWS: u32 = 20_000;
const GRID_COLS: u32 = 200;
const ROW_H: f32 = 20.0;
const HEAD_H: f32 = 22.0;
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
            .with_title("Обезличивание Excel")
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

/// Открытый файл: своя книга, свои правила. Генератор фейков и ключ — общие для всех файлов.
struct Doc {
    path: PathBuf,
    wb: Workbook,
    grids: HashMap<usize, Grid>,
    widths: HashMap<usize, Vec<f32>>,
    cur_sheet: usize,
    /// Правила этого файла (настройки общие — `App::settings`).
    profile: Profile,
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
        let mut app = App {
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
            preview: Vec::new(),
            preview_rules: Vec::new(),
            preview_dirty: false,
            preview_job: None,
            cursor: None,
            last_vp: Rect::NOTHING,
            ensure_visible: false,
            grid_id: None,
        };
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
        app.open_files(files, false);
        app
    }

    fn set_status(&mut self, s: impl Into<String>, err: bool) {
        self.status = s.into();
        self.status_err = err;
    }

    fn doc(&self) -> Option<&Doc> {
        self.docs.get(self.cur)
    }

    /// Открывает файлы; `replace` — закрыть уже открытые (новый набор файлов).
    fn open_files(&mut self, paths: Vec<PathBuf>, replace: bool) {
        if paths.is_empty() {
            return;
        }
        if replace {
            self.docs.clear();
            self.key_in = None;
            self.key_chosen = false;
        }
        let mut notes: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut last = None;
        for path in paths {
            if let Some(i) = self.docs.iter().position(|d| d.path == path) {
                last = Some(i);
                continue;
            }
            let wb = match Workbook::open(&path) {
                Ok(wb) => wb,
                Err(e) => {
                    errors.push(format!("{}: {e:#}", file_name(&path)));
                    continue;
                }
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
            self.docs.push(Doc { path, wb, grids: HashMap::new(), widths: HashMap::new(), cur_sheet: 0, profile });
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
        let files: Vec<(PathBuf, Vec<Rule>)> = self.docs.iter().map(|d| (d.path.clone(), d.profile.rules.clone())).collect();
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
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Done(r, p) => finished = Some((r, p)),
            }
        }
        if let Some((r, profiles)) = finished {
            self.job = None;
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
                        self.set_status("Готово, но проверка нашла остатки реальных значений — см. журнал внизу", true);
                    } else if many {
                        let dir = reports[0].output.parent().map(|d| d.display().to_string()).unwrap_or_default();
                        self.set_status(format!("Готово: файлов {} в {dir}", reports.len()), false);
                    } else {
                        self.set_status(format!("Готово: {}", reports[0].output.display()), false);
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
        if self.docs.is_empty() {
            return;
        }
        let empty: Vec<String> = self.docs.iter().filter(|d| d.profile.rules.is_empty()).map(|d| file_name(&d.path)).collect();
        if !empty.is_empty() {
            self.set_status(format!("Нет правил в файлах: {} — добавьте правила или уберите файлы из списка", empty.join(", ")), true);
            return;
        }
        let inputs: Vec<PathBuf> = self.docs.iter().map(|d| d.path.clone()).collect();
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
            self.docs.iter().map(|d| Profile { rules: d.profile.rules.clone(), settings: self.settings.clone() }).collect();
        if self.save_profile {
            for (d, p) in self.docs.iter().zip(&profiles) {
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
        self.set_status("Обработка…", false);
        std::thread::spawn(move || {
            let tx_log = tx.clone();
            let ctx_log = ctx.clone();
            let res = run_batch(&batch, &mut |m| {
                let _ = tx_log.send(Msg::Log(m));
                ctx_log.request_repaint();
            });
            let _ = tx.send(Msg::Done(res.map_err(|e| format!("{e:#}")), profiles));
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------ панели

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Открыть Excel…").on_hover_text("Можно выбрать сразу несколько файлов одного заказчика").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Excel", &["xlsx", "xlsm"]).pick_files() {
                    self.open_files(p, true);
                }
            }
            if ui
                .add_enabled(!self.docs.is_empty(), egui::Button::new("Добавить файл…"))
                .on_hover_text("Ещё файл в тот же набор: одни и те же люди, организации и ИНН получат одни и те же фейки во всех файлах")
                .clicked()
            {
                if let Some(p) = rfd::FileDialog::new().add_filter("Excel", &["xlsx", "xlsm"]).pick_files() {
                    self.open_files(p, false);
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
                ui.heading(format!("Файлы ({})", self.docs.len()));
                if self.docs.len() > 1 {
                    ui.label(RichText::new("Один человек, организация, ИНН — один фейк во всех файлах набора").small().weak());
                }
                egui::ScrollArea::vertical().id_salt("files").auto_shrink([false, false]).show(ui, |ui| {
                    for (i, d) in self.docs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("✕").on_hover_text("Убрать файл из набора (сам файл не удаляется)").clicked() {
                                close = Some(i);
                            }
                            let label = format!("{}  [{}]", file_name(&d.path), d.profile.rules.len());
                            if ui.selectable_label(self.cur == i, label).on_hover_text(d.path.display().to_string()).clicked() {
                                pick = Some(i);
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
            ui.add(egui::TextEdit::singleline(&mut self.range_text).desired_width(110.0).hint_text("C5:C120"))
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
        let border = Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
        let font = FontId::proportional(13.0);
        let shift = ui.input(|i| i.modifiers.shift);
        let press_origin = ui.input(|i| i.pointer.press_origin());

        // Клавиатура как в Excel: стрелки, Shift — выделение вверх-вниз, Ctrl — к краю данных,
        // PageUp/PageDown, Home, Ctrl+Home/End. Не мешаем вводу в полях.
        let grid_focused = self.grid_id.is_some_and(|id| ui.memory(|m| m.has_focus(id)));
        let free = ui.memory(|m| m.focused().is_none());
        if (grid_focused || free) && !ui.ctx().wants_keyboard_input() && ncols > 0 && nrows > 0 {
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

            // Закреплённая строка с буквами колонок.
            painter.rect_filled(Rect::from_min_size(vis.min, vec2(vis.width(), HEAD_H)), 0.0, visuals.extreme_bg_color);
            for c in c0..c1 {
                let col = c as u32;
                let cr = Rect::from_min_size(pos2(o.x + ROWNUM_W + xs[c], vis.min.y), vec2(widths[c], HEAD_H));
                if sel.is_some_and(|(sc, _, _)| sc == col) {
                    painter.rect_filled(cr, 0.0, sel_fill);
                }
                painter.text(cr.center(), Align2::CENTER_CENTER, col_name(col), font.clone(), visuals.strong_text_color());
                painter.vline(cr.max.x, cr.y_range(), line);
            }
            painter.hline(vis.x_range(), vis.min.y + HEAD_H, border);

            // Закреплённая колонка с номерами строк.
            let nums = Rect::from_min_size(pos2(vis.min.x, vis.min.y + HEAD_H), vec2(ROWNUM_W, (vis.height() - HEAD_H).max(0.0)));
            painter.rect_filled(nums, 0.0, visuals.extreme_bg_color);
            let np = painter.with_clip_rect(nums);
            for ri in r0..r1 {
                let row = ri + 1;
                let rr = Rect::from_min_size(pos2(vis.min.x, o.y + HEAD_H + ri as f32 * ROW_H), vec2(ROWNUM_W, ROW_H));
                if sel.is_some_and(|(_, a, b)| row >= a && row <= b) {
                    np.rect_filled(rr, 0.0, sel_fill);
                }
                np.text(pos2(rr.max.x - 6.0, rr.center().y), Align2::RIGHT_CENTER, row.to_string(), font.clone(), visuals.weak_text_color());
            }
            painter.vline(vis.min.x + ROWNUM_W, vis.y_range(), border);
            painter.rect_filled(Rect::from_min_size(vis.min, vec2(ROWNUM_W, HEAD_H)), 0.0, visuals.extreme_bg_color);

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
                            tip.push_str(&format!("\nПравило {} — {}", x.range_text(), x.kind.title()));
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
        let old = self.sel;
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
        let cur_sel = self.sel.map(|s| (self.sheet_name(doc.cur_sheet), s));
        let sel_last = cur_sel.as_ref().and_then(|(sh, _)| max_rows.get(sh).copied());
        let preview_rules = self.preview_rules.get(self.cur);
        let counting = self.preview_job.is_some();
        let no_preview = Vec::new();
        let preview = self.preview.get(self.cur).unwrap_or(&no_preview);
        let mut take = None;
        // Правило, которому нужно подобрать подпись шаблона (сменили тип или диапазон).
        let mut refill = None;
        let doc = &mut self.docs[self.cur];
        egui::ScrollArea::vertical().id_salt(("rules", self.cur)).auto_shrink([false, false]).show(ui, |ui| {
            if doc.profile.rules.is_empty() {
                ui.label("Правил нет. Выделите диапазон в таблице и нажмите «Добавить правило» или используйте автоподбор.");
            }
            for (i, r) in doc.profile.rules.iter_mut().enumerate() {
                // Предпросмотр, посчитанный для этого же правила (правку ещё считают — не показываем старое).
                let pv = preview.get(i).filter(|_| preview_rules.and_then(|v| v.get(i)) == Some(&*r));
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.strong(format!("{}!{}", r.sheet, r.range_text()));
                        match pv {
                            Some(p) => {
                                ui.label(RichText::new(format!("≈ {} ячеек", p.cells)).weak());
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
                    ui.horizontal(|ui| {
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
                    });
                    ui.horizontal(|ui| {
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
            }
        });
        if let Some(i) = remove {
            doc.profile.rules.remove(i);
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
            let can = !self.docs.is_empty() && !running;
            let label = if self.docs.len() > 1 { format!("Обезличить все файлы ({})…", self.docs.len()) } else { "Обезличить и сохранить…".into() };
            if ui.add_enabled(can, egui::Button::new(RichText::new(label).strong().size(16.0))).clicked() {
                self.start_job(ui.ctx());
            }
            if running {
                ui.spinner();
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for l in &self.log {
                let mut t = RichText::new(l).monospace();
                if l.starts_with("Ошибка") || l.starts_with("Проверка: реальные") || l.starts_with("Внимание") {
                    t = t.color(Color32::from_rgb(220, 80, 60));
                }
                ui.label(t);
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_job();
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).filter(|p| is_excel(p)).collect());
        if !dropped.is_empty() {
            self.open_files(dropped, false);
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
