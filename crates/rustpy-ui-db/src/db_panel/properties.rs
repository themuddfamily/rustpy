use super::workspace::*;
use crate::a11y::AccessibleCombo;
use rustpy_db::workspace::*;
impl DatabaseWorkspace {
    pub(super) fn properties_ui(&mut self, ui: &mut egui::Ui) {
        if !self.properties {
            return;
        }
        egui::CollapsingHeader::new("Data Source Properties → Options → Introspection").default_open(true).show(ui,|ui|{
        ui.horizontal(|ui|{let name_label=ui.label("Name");ui.add(egui::TextEdit::singleline(&mut self.draft.name).id_salt("database-source-name")).labelled_by(name_label.id);egui::ComboBox::from_id_salt("database-dialect").selected_text(format!("{:?}",self.draft.dialect)).show_ui(ui,|ui|{for d in Dialect::ALL{ui.selectable_value(&mut self.draft.dialect,*d,format!("{d:?}"));}}).combo_named(crate::i18n::tr_static("Dialect"));ui.checkbox(&mut self.draft.ddl_only,"Offline DDL data source");});
        if self.draft.dialect==Dialect::Sqlite||self.draft.ddl_only{let mut file=self.draft.file.display().to_string();ui.horizontal(|ui|{let file_label=ui.label("File");if ui.add(egui::TextEdit::singleline(&mut file).id_salt("database-source-file")).labelled_by(file_label.id).changed(){self.draft.file=file.into();}});}else{
            let native=matches!(self.draft.dialect,Dialect::Mongo|Dialect::Redis);
            if native {
                ui.label("Redis / MongoDB connection. Changes apply immediately. Credentials are used only for this session.");
                if self.draft.port==0 {self.draft.port=if self.draft.dialect==Dialect::Redis {6379}else{27017};}
            } else {
            ui.label("External SQL sources use a registered native ODBC driver. Cassandra needs a vendor SQL/ODBC driver. H2 and Big Data Tools have no built-in adapter.");
            ui.horizontal(|ui|{ui.label("Driver");ui.text_edit_singleline(&mut self.draft.driver);if ui.button("List installed drivers").clicked()&&self.drivers.is_none(){let(tx,rx)=std::sync::mpsc::sync_channel(1);self.drivers=Some(rx);let _=std::thread::Builder::new().name("database-drivers".into()).spawn(move||{let result=(||->anyhow::Result<Vec<String>>{let env=odbc_api_for_ui_drivers()?;Ok(env)})().map_err(|e|e.to_string());let _=tx.send(result);});}});
            for name in &self.driver_names{if ui.selectable_label(self.draft.driver==*name,name).clicked(){self.draft.driver=name.clone();}}
            }
            ui.horizontal(|ui|{ui.label("Host");ui.text_edit_singleline(&mut self.draft.host);ui.label("Port");ui.add(egui::DragValue::new(&mut self.draft.port));});ui.horizontal(|ui|{ui.label("Database / project");ui.text_edit_singleline(&mut self.draft.database);ui.label("User");ui.text_edit_singleline(&mut self.draft.user);});
            ui.horizontal(|ui|{ui.label("Session password / token");ui.add(egui::TextEdit::singleline(&mut self.secret).password(true));});
            if native {ui.label("Connection options (JSON string values): tls, tlsCAFile; Redis tlsClientCert + tlsClientKey; Mongo tlsCertificateKeyFile + authSource. SSH, proxy routing and shared certificate acceptance are not yet supported for these connections. Never put passwords/tokens in options.");}
            else {ui.label("Driver options as JSON: SSL certificate paths, verification, warehouse, authentication mode, DSN. Never store passwords/tokens here. Exact option names are driver-specific.");}
            ui.add(egui::TextEdit::multiline(&mut self.options).code_editor().desired_rows(3));
            let mut tunnel=self.draft.ssh.is_some();if ui.add_enabled(!native||tunnel,egui::Checkbox::new(&mut tunnel,"SSH tunnel (strict known-host verification, key/agent authentication)")).changed(){self.draft.ssh=tunnel.then(Default::default);}
if let Some(t)=&mut self.draft.ssh{
    let mut program=t.ssh_program.display().to_string();
    ui.horizontal(|ui|{let label=ui.label("SSH executable (optional)");
        if ui.add(egui::TextEdit::singleline(&mut program).id_salt("database-ssh-executable").hint_text("Empty uses ssh on PATH")).labelled_by(label.id).changed(){t.ssh_program=program.into();}});
    let mut config=t.config_file.as_ref().map(|path|path.display().to_string()).unwrap_or_default();
    ui.horizontal(|ui|{let label=ui.label("SSH config file (optional)");
        if ui.add(egui::TextEdit::singleline(&mut config).id_salt("database-ssh-config").hint_text("Empty uses the OpenSSH default")).labelled_by(label.id).changed(){t.config_file=(!config.is_empty()).then(||config.into());}});
    ui.horizontal(|ui|{ui.label("user@host");ui.text_edit_singleline(&mut t.destination);ui.label("Local port");ui.add(egui::DragValue::new(&mut t.local_port));});ui.horizontal(|ui|{ui.label("Remote DB host");ui.text_edit_singleline(&mut t.remote_host);ui.label("Remote port");ui.add(egui::DragValue::new(&mut t.remote_port));});let mut identity=t.identity.display().to_string();if ui.add(egui::TextEdit::singleline(&mut identity).hint_text("SSH identity file (optional)")).changed(){t.identity=identity.into();}let mut known=t.known_hosts.display().to_string();if ui.add(egui::TextEdit::singleline(&mut known).hint_text("Known hosts file (optional; OpenSSH default otherwise)")).changed(){t.known_hosts=known.into();}}
        }
        ui.checkbox(&mut self.draft.read_only,"Read-only");let mut schemas=self.draft.schemas.join(",");ui.horizontal(|ui|{ui.label("Schemas… (comma separated; empty = all)");if ui.text_edit_singleline(&mut schemas).changed(){self.draft.schemas=schemas.split(',').map(str::trim).filter(|s|!s.is_empty()).map(str::to_string).collect();}});ui.horizontal(|ui|{ui.label("Introspection object limit");ui.add(egui::DragValue::new(&mut self.draft.introspection_depth).range(0..=10000));ui.label("0 uses the workspace limit");});
        ui.horizontal(|ui|{if ui.button("Save properties").clicked(){if !self.options.trim().is_empty(){match serde_json::from_str(&self.options){Ok(v)=>self.draft.options=v,Err(e)=>{self.last_error=e.to_string();return;}}}
if (self.draft.dialect==Dialect::Sqlite||self.draft.ddl_only)&&self.draft.file.is_relative(){if let Some(root)=&self.root{self.draft.file=root.join(&self.draft.file);}}
if let Some(tunnel)=&self.draft.ssh{if let Err(error)=validate_tunnel_paths(tunnel){self.last_error=error;return;}}
if let Err(e)=self.draft.validate(){self.last_error=e.to_string();return;}
if let Some(old)=self.model.sources.iter_mut().find(|s|s.id==self.draft.id){*old=self.draft.clone();}else{self.model.sources.push(self.draft.clone());}self.selected=Some(self.draft.id);self.save();self.properties=false;}
if ui.button("Close").clicked(){self.properties=false;}});
    });
    }
    pub(super) fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Database → Settings").default_open(true).show(ui,|ui|{
        let s=&mut self.model.settings;ui.horizontal(|ui|{ui.label("Result limit");ui.add(egui::DragValue::new(&mut s.result_limit).range(1..=100000));ui.label("Show first N / page");ui.add(egui::DragValue::new(&mut s.page_size).range(1..=10000));ui.label("Timeout seconds");ui.add(egui::DragValue::new(&mut s.timeout_seconds).range(1..=300));});ui.checkbox(&mut s.auto_commit,"Auto-commit for new consoles (transaction control remains per console)");ui.checkbox(&mut s.allow_local,"Enable local data sources");ui.horizontal(|ui|{ui.label("User parameter prefix");ui.text_edit_singleline(&mut s.parameter_prefix);});for (label,path) in [("Scripts directory",&mut s.scripts_directory),("Extractors directory",&mut s.extractors_directory)]{let mut text=path.display().to_string();ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));if ui.text_edit_singleline(&mut text).changed(){*path=text.into();}});}
        ui.separator();ui.label("SQL dialect / resolution scopes (longest file or folder path wins)");ui.text_edit_singleline(&mut self.scope_path);if ui.button("Assign selected source to scope").clicked(){if let Some(source)=self.model.sources.iter().find(|s|Some(s.id)==self.selected){let path=self.root.as_ref().map(|r|r.join(&self.scope_path)).unwrap_or_else(||self.scope_path.clone().into());self.model.settings.scopes.retain(|s|s.path!=path);self.model.settings.scopes.push(Scope{path,source:source.id,dialect:source.dialect});}}let mut remove=None;for(index,scope)in self.model.settings.scopes.iter().enumerate(){ui.horizontal(|ui|{ui.label(format!("{} → {:?} / source {}",scope.path.display(),scope.dialect,scope.source));if ui.small_button("Remove").clicked(){remove=Some(index);}});}
if let Some(index)=remove{self.model.settings.scopes.remove(index);}
        ui.label("Custom extractors: JSON array of {name, program, arguments}. Programs receive exported JSON on stdin, write extracted output to stdout; explicit Run only.");let mut extractors=serde_json::to_string_pretty(&self.model.settings.extractors).unwrap_or_default();if ui.add(egui::TextEdit::multiline(&mut extractors).desired_rows(3)).changed(){match serde_json::from_str(&extractors){Ok(v)=>self.model.settings.extractors=v,Err(e)=>self.last_error=e.to_string()}}
        if ui.button("Save database settings").clicked(){self.save();}});
    }
}
fn validate_tunnel_paths(tunnel: &Tunnel) -> Result<(), String> {
    for (name, path) in [
        (
            "SSH executable",
            (!tunnel.ssh_program.as_os_str().is_empty()).then_some(tunnel.ssh_program.as_path()),
        ),
        ("SSH config file", tunnel.config_file.as_deref()),
    ] {
        if let Some(path) = path {
            let Some(text) = path.to_str() else {
                return Err(format!(
                    "{name} cannot be persisted as a UTF-8 database setting; the original path has been preserved"
                ));
            };
            if text.chars().any(char::is_control) || !path.is_absolute() || !path.is_file() {
                return Err(format!(
                    "Choose an existing absolute {name} path without control characters"
                ));
            }
        }
    }
    Ok(())
}
fn odbc_api_for_ui_drivers() -> anyhow::Result<Vec<String>> {
    rustpy_db::session::installed_drivers()
}

#[cfg(test)]
pub(super) mod ui_fixture {
    use super::*;
    use crate::db_panel::DbPanel;
    use std::time::{Duration, Instant};

    pub(crate) struct Fixture {
        pub panel: DbPanel,
        pub ctx: egui::Context,
        pub root: tempfile::TempDir,
        pub nodes: std::collections::HashMap<egui::accesskit::NodeId, egui::accesskit::Node>,
        root_node: Option<egui::accesskit::NodeId>,
    }
    impl Fixture {
        pub fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let mut panel = DbPanel::default();
            panel.workspace.load(root.path());
            panel.set_database_execution_allowed(false);
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            ctx.enable_accesskit();
            ctx.all_styles_mut(|style| style.animation_time = 0.0);
            Self {
                panel,
                ctx,
                root,
                nodes: Default::default(),
                root_node: None,
            }
        }
        pub fn frame(&mut self, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
            let output = self.ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(2000.0, 2600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let _ = self.panel.workspace_ui(ui, self.root.path());
                    });
                },
            );
            let tree = output.platform_output.accesskit_update.clone();
            // Retire atlas deltas before any fixture assertion can unwind.
            output.drop_without_applying_deltas();
            let tree = tree.expect("actual database panel accessibility tree");
            self.nodes.extend(tree.nodes.iter().cloned());
            if let Some(tree) = &tree.tree {
                self.root_node = Some(tree.root);
            }
            let mut todo = self.root_node.into_iter().collect::<Vec<_>>();
            let mut reachable = std::collections::HashSet::new();
            while let Some(id) = todo.pop() {
                if !reachable.insert(id) {
                    continue;
                }
                if let Some(node) = self.nodes.get(&id) {
                    todo.extend_from_slice(node.children());
                }
            }
            self.nodes.retain(|id, _| reachable.contains(id));
            tree
        }
        fn click_point(&mut self, point: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            self.frame(vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            self.frame(vec![]);
        }
        pub fn activate(&mut self, target: &str) {
            self.frame(vec![]);
            let bounds = self
                .nodes
                .values()
                .find(|node| {
                    node.label() == Some(target)
                        || node.labelled_by().iter().any(|id| {
                            self.nodes
                                .get(id)
                                .is_some_and(|label| label.value() == Some(target))
                        })
                })
                .and_then(|node| node.bounds())
                .unwrap_or_else(|| panic!("actual visible control {target}"));
            self.click_point(egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            ));
        }
        pub fn replace(&mut self, target: &str, text: &str) {
            self.activate(target);
            self.frame(key(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            ));
            self.frame(key(egui::Key::Backspace, egui::Modifiers::NONE));
            if !text.is_empty() {
                self.frame(vec![egui::Event::Text(text.into())]);
            }
        }
        pub fn replace_parameter(&mut self, name: &str, text: &str) {
            self.frame(vec![]);
            let label = self
                .nodes
                .values()
                .find(|node| node.value() == Some(name))
                .and_then(|node| node.bounds())
                .expect("actual parameter label bounds");
            let input = self
                .nodes
                .values()
                .filter(|node| node.role() == egui::accesskit::Role::TextInput)
                .filter_map(|node| node.bounds())
                .filter(|bounds| {
                    bounds.x0 >= label.x1
                        && bounds.y0 <= (label.y0 + label.y1) / 2.0
                        && bounds.y1 >= (label.y0 + label.y1) / 2.0
                })
                .min_by(|a, b| a.x0.total_cmp(&b.x0))
                .expect("actual adjacent parameter input");
            let point = egui::pos2(
                ((input.x0 + input.x1) / 2.0) as f32,
                ((input.y0 + input.y1) / 2.0) as f32,
            );
            self.click_point(point);
            self.frame(key(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            ));
            self.frame(key(egui::Key::Backspace, egui::Modifiers::NONE));
            self.frame(vec![egui::Event::Text(text.into())]);
        }
        pub fn ready(&mut self) {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                self.frame(vec![]);
                if self.panel.workspace.owners.iter().all(|c| !c.owner.busy) {
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "actual owned SQLite session must finish its request"
                );
                std::thread::yield_now();
            }
        }
        pub fn retire(&mut self) {
            self.panel.stop_all_database();
            let until = Instant::now() + Duration::from_secs(5);
            while self.panel.database_has_owners() && Instant::now() < until {
                self.panel.poll_database();
                std::thread::yield_now();
            }
            assert!(
                !self.panel.database_has_owners(),
                "all retained database workers must join"
            );
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let already_unwinding = std::thread::panicking();
            self.panel.stop_all_database();
            let until = Instant::now() + Duration::from_secs(5);
            let mut overdue = false;
            // Keep every fixture-owned session (including explicitly admitted
            // native providers), the panel's
            // JoinHandles and its TempDir alive until polling actually joins
            // them, including when a control/assertion failed mid-request.
            // A coordinator timeout may terminate the whole test process, but
            // dropping a JoinHandle here must never detach a worker and remove
            // source files underneath it.
            while self.panel.database_has_owners() {
                self.panel.poll_database();
                if !overdue && Instant::now() >= until {
                    overdue = true;
                    eprintln!(
                        "database fixture cleanup exceeded 5s; retaining worker and source ownership until physical retirement"
                    );
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            // Report a clean-path timeout only after all workers retired. An
            // existing test failure remains the sole panic during unwinding.
            assert!(
                already_unwinding || !overdue,
                "database fixture cleanup exceeded 5s; all workers have now joined"
            );
        }
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }
    fn properties(f: &mut Fixture) {
        f.panel.workspace.draft = f.panel.workspace.model.sources[0].clone();
        f.panel.workspace.properties = true;
        f.frame(vec![]);
        f.frame(vec![]);
    }
    #[test]
    fn published_ssh_paths_roundtrip_and_clear_to_legacy_defaults_without_execution() {
        let mut f = Fixture::new();
        let program = f.root.path().join("selected ssh π");
        let config = f.root.path().join("private ssh config π");
        std::fs::write(&program, b"not executed").unwrap();
        std::fs::write(&config, b"Host isolated\n").unwrap();
        f.panel.workspace.model.sources.push(Source {
            id: 7,
            name: "private connection".into(),
            dialect: Dialect::Postgres,
            ssh: Some(Tunnel {
                destination: "developer@isolated".into(),
                local_port: 15432,
                remote_host: "database.internal".into(),
                remote_port: 5432,
                ..Default::default()
            }),
            ..Default::default()
        });
        f.panel.workspace.secret = "session-only-never-persist".into();
        properties(&mut f);
        f.replace("SSH executable (optional)", program.to_str().unwrap());
        f.replace("SSH config file (optional)", config.to_str().unwrap());
        f.activate("Save properties");
        assert!(
            !f.panel.workspace.properties,
            "{}",
            f.panel.workspace.last_error
        );
        let saved = Workspace::load(f.root.path());
        let tunnel = saved.sources[0].ssh.as_ref().unwrap();
        assert_eq!(tunnel.ssh_program, program);
        assert_eq!(tunnel.config_file, Some(config));
        assert_eq!(tunnel.destination, "developer@isolated");
        assert_eq!(tunnel.remote_host, "database.internal");
        let persisted =
            std::fs::read_to_string(f.root.path().join(".rustpy/database-workspace.json")).unwrap();
        assert!(!persisted.contains("session-only-never-persist"));
        assert!(
            !f.panel.database_has_owners(),
            "editing untrusted properties never starts a provider"
        );
        properties(&mut f);
        f.replace("SSH executable (optional)", "");
        f.replace("SSH config file (optional)", "");
        f.activate("Save properties");
        let saved = Workspace::load(f.root.path());
        let tunnel = saved.sources[0].ssh.as_ref().unwrap();
        assert!(tunnel.ssh_program.as_os_str().is_empty());
        assert_eq!(tunnel.config_file, None);
        assert_eq!(std::fs::read(program).unwrap(), b"not executed");
        f.retire();
    }
    #[test]
    fn rejected_selected_paths_preserve_saved_connection_and_native_path_identity() {
        let mut f = Fixture::new();
        f.panel.workspace.model.sources.push(Source {
            id: 1,
            dialect: Dialect::Postgres,
            ssh: Some(Tunnel::default()),
            ..Default::default()
        });
        f.panel.workspace.save();
        let path = f.root.path().join(".rustpy/database-workspace.json");
        let original = std::fs::read(&path).unwrap();
        properties(&mut f);
        f.replace("SSH config file (optional)", "relative/config");
        f.activate("Save properties");
        assert!(f.panel.workspace.properties);
        assert!(f
            .panel
            .workspace
            .last_error
            .contains("existing absolute SSH config file"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let missing = f.root.path().join("missing-config");
        f.replace("SSH config file (optional)", missing.to_str().unwrap());
        f.activate("Save properties");
        assert!(f.panel.workspace.properties);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let original_path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
                b"/native/non-utf8-\xff".to_vec(),
            ));
            f.panel.workspace.draft.ssh.as_mut().unwrap().ssh_program = original_path.clone();
            f.frame(vec![]);
            assert_eq!(
                f.panel.workspace.draft.ssh.as_ref().unwrap().ssh_program,
                original_path,
                "rendering must never replace a native path with its lossy display"
            );
        }
        let invalid = Tunnel {
            config_file: Some(std::path::PathBuf::from("/private/control\npath")),
            ..Default::default()
        };
        assert!(validate_tunnel_paths(&invalid).is_err());
        f.retire();
    }
}
