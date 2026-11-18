use super::*;

use michost_host_abi::PluginDescriptor;

impl MicHostApp {
    pub(crate) fn poll_pending_add(&mut self) {
        let Some(pending) = self.pending_add.take() else {
            return;
        };
        match pending.receiver.try_recv() {
            Ok(Ok(prepared)) => {
                let running = self.state == EngineState::Running
                    && self.session.is_some()
                    && self.session_epoch == pending.session_epoch;
                if running {
                    let position = if let Some(source_id) = pending.insert_after {
                        match self
                            .project
                            .chain
                            .iter()
                            .position(|entry| entry.id == source_id)
                        {
                            Some(index) => index + 1,
                            None => {
                                if let Some(backend) = self.backend.clone() {
                                    discard_prepared(&backend, prepared);
                                }
                                self.last_error =
                                    Some("the plugin to duplicate was removed".to_string());
                                return;
                            }
                        }
                    } else {
                        self.project.chain.len()
                    };
                    let source_state = pending
                        .insert_after
                        .and_then(|source_id| self.project.plugin_states.get(&source_id).cloned());
                    let id = self.allocate_plugin_id();
                    let mut entry = pending.template.unwrap_or(ChainEntry {
                        id,
                        plugin: pending.spec,
                        bypassed: false,
                        sidechain_source: SidechainSource::Off,
                        oversampling: 1,
                    });
                    entry.id = id;
                    entry.plugin.main_inputs = prepared.main_inputs;
                    entry.plugin.main_outputs = prepared.main_outputs;
                    entry.plugin.sidechain_inputs = prepared.native_sidechain;
                    self.project.chain.insert(position, entry);
                    if let Some(state) = source_state {
                        self.project.plugin_states.insert(id, state);
                    }
                    let inserted = self.session.as_mut().map(|session| {
                        session.insert_prepared_with_project(&self.project, id, prepared)
                    });
                    if let Some(Err(error)) = inserted {
                        self.project.chain.retain(|entry| entry.id != id);
                        self.project.plugin_states.remove(&id);
                        self.last_error = Some(error);
                        return;
                    }
                    self.mark_dirty();
                    self.save_now();
                    return;
                }
                if let Some(backend) = self.backend.clone() {
                    discard_prepared(&backend, prepared);
                }
            }
            Ok(Err(error)) => {
                self.last_error = Some(error);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.pending_add = Some(pending);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.last_error = Some(format!(
                    "loading '{}': the plugin worker stopped",
                    pending.name
                ));
            }
        }
    }

    pub(crate) fn begin_duplicate(&mut self, index: usize) {
        if self.pending_add.is_some() {
            return;
        }
        let Some(source) = self.project.chain.get(index).cloned() else {
            return;
        };
        let source_state = self.project.plugin_states.get(&source.id).cloned();

        if let Some(kind) = BuiltinKind::from_path(&source.plugin.path) {
            if !cfg!(debug_assertions) {
                self.last_error =
                    Some("built-in effects are not included in release builds".to_string());
                return;
            }
            if self.session_worker.is_some() {
                self.stop_audio();
            }
            let id = self.allocate_plugin_id();
            let mut entry = source.clone();
            entry.id = id;
            self.project.chain.insert(index + 1, entry);
            if let Some(state) = source_state.clone() {
                self.project.plugin_states.insert(id, state);
            }
            if self.state == EngineState::Running
                && let Some(session) = self.session.as_mut()
            {
                let params = ParamSet::new(kind);
                if let Some(state) = &source_state {
                    let _ = params.decode(&state.component);
                }
                if let Err(error) = session.insert_builtin_with_project(
                    &self.project,
                    id,
                    kind,
                    params,
                    kind.name(),
                ) {
                    self.project.chain.retain(|entry| entry.id != id);
                    self.project.plugin_states.remove(&id);
                    self.last_error = Some(error);
                    return;
                }
            }
            self.mark_dirty();
            self.save_now();
            return;
        }

        if self.state == EngineState::Running && self.session.is_some() {
            let Some(backend) = self.backend.clone() else {
                self.last_error = Some("mic_host.dll is not loaded".to_string());
                return;
            };
            let Some(session) = self.session.as_ref() else {
                return;
            };
            let sample_rate = session.sample_rate();
            let block_size = session.block_size();
            let oversampling = michost_graph::normalize_oversampling(source.oversampling);
            let path = source.plugin.path.clone();
            let class_id = source.plugin.class_id.clone();
            let name = source.plugin.name.clone();
            let sidechain = if source.sidechain_source != SidechainSource::Off {
                source.plugin.sidechain_inputs
            } else {
                0
            };
            let state = source_state.map(|state| state.component);
            let (sender, receiver) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("michost-duplicate".to_string())
                .spawn(move || {
                    let result = prepare_plugin_for_insert_with_oversampling(
                        backend,
                        &path,
                        &class_id,
                        &name,
                        sample_rate,
                        block_size,
                        oversampling,
                        sidechain,
                        state.as_deref(),
                    );
                    let _ = sender.send(result);
                });
            match spawned {
                Ok(_) => {
                    self.pending_add = Some(PendingAdd {
                        name: source.plugin.name.clone(),
                        spec: source.plugin.clone(),
                        session_epoch: self.session_epoch,
                        receiver,
                        insert_after: Some(source.id),
                        template: Some(source),
                    });
                }
                Err(error) => {
                    self.last_error = Some(format!("starting the plugin worker: {error}"));
                }
            }
            return;
        }

        if self.session_worker.is_some() {
            self.stop_audio();
        }
        let id = self.allocate_plugin_id();
        let mut entry = source.clone();
        entry.id = id;
        self.project.chain.insert(index + 1, entry);
        if let Some(state) = source_state {
            self.project.plugin_states.insert(id, state);
        }
        self.mark_dirty();
        self.save_now();
    }

    pub(crate) fn begin_add(&mut self, descriptor: &PluginDescriptor) {
        if self.pending_add.is_some() {
            return;
        }
        if let Some(kind) = BuiltinKind::from_path(&descriptor.path) {
            self.begin_add_builtin(kind);
            return;
        }
        if self.state == EngineState::Running && self.session.is_some() {
            let Some(backend) = self.backend.clone() else {
                self.last_error = Some("mic_host.dll is not loaded".to_string());
                return;
            };
            let Some(session) = self.session.as_ref() else {
                return;
            };
            let sample_rate = session.sample_rate();
            let block_size = session.block_size();
            let name = descriptor.name.clone();
            let path = descriptor.path.clone();
            let class_id = descriptor.class_id.clone();
            let spec = descriptor.to_spec();
            let (sender, receiver) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("michost-insert".to_string())
                .spawn(move || {
                    let result = prepare_plugin_for_insert(
                        backend,
                        &path,
                        &class_id,
                        &name,
                        sample_rate,
                        block_size,
                        0,
                        None,
                    );
                    let _ = sender.send(result);
                });
            match spawned {
                Ok(_) => {
                    self.pending_add = Some(PendingAdd {
                        name: descriptor.name.clone(),
                        spec,
                        session_epoch: self.session_epoch,
                        receiver,
                        insert_after: None,
                        template: None,
                    });
                }
                Err(error) => {
                    self.last_error = Some(format!("starting the plugin worker: {error}"));
                }
            }
            return;
        }
        if self.session_worker.is_some() {
            self.stop_audio();
        }
        let id = self.allocate_plugin_id();
        self.project.chain.push(ChainEntry {
            id,
            plugin: descriptor.to_spec(),
            bypassed: false,
            sidechain_source: SidechainSource::Off,
            oversampling: 1,
        });
        self.mark_dirty();
        self.save_now();
    }

    pub(crate) fn begin_add_builtin(&mut self, kind: BuiltinKind) {
        if !cfg!(debug_assertions) {
            self.last_error =
                Some("built-in effects are not included in release builds".to_string());
            return;
        }
        if self.pending_add.is_some() {
            return;
        }
        if self.session_worker.is_some() {
            self.stop_audio();
        }
        let id = self.allocate_plugin_id();
        let params = ParamSet::new(kind);
        self.project.chain.push(ChainEntry {
            id,
            plugin: builtin_spec(kind),
            bypassed: false,
            sidechain_source: SidechainSource::Off,
            oversampling: 1,
        });
        self.project.set_plugin_state(id, params.encode());
        if self.state == EngineState::Running
            && let Some(session) = self.session.as_mut()
            && let Err(error) =
                session.insert_builtin_with_project(&self.project, id, kind, params, kind.name())
        {
            self.project.chain.retain(|entry| entry.id != id);
            self.project.remove_plugin_state(id);
            self.last_error = Some(error);
            return;
        }
        self.mark_dirty();
        self.save_now();
    }

    pub(crate) fn poll_scan(&mut self) {
        let Some(receiver) = self.scan.receiver.take() else {
            return;
        };
        let mut keep = true;
        let mut done = None;
        let mut error = None;
        let mut warning = None;
        loop {
            match receiver.try_recv() {
                Ok(ScanUpdate::Progress {
                    done: current,
                    total,
                }) => {
                    self.scan.done = current;
                    self.scan.total = total;
                }
                Ok(ScanUpdate::Warning(message)) => {
                    warning = Some(message);
                }
                Ok(ScanUpdate::Done(descriptors)) => {
                    done = Some(descriptors);
                    keep = false;
                    break;
                }
                Ok(ScanUpdate::Error(message)) => {
                    error = Some(message);
                    keep = false;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep = false;
                    break;
                }
            }
        }
        if let Some(message) = warning {
            self.scan.error = Some(message);
        }
        if let Some(descriptors) = done {
            self.plugins = descriptors;
            self.plugin_generation = self.plugin_generation.wrapping_add(1);
            self.scan.active = false;
            self.scan.done = self.scan.total;
            logging::line(format!(
                "plugin scan finished: {} plugins",
                self.plugins.len()
            ));
        } else if let Some(error) = error {
            self.scan.active = false;
            self.scan.error = Some(error.clone());
            logging::line(error);
        } else if keep {
            self.scan.receiver = Some(receiver);
        } else {
            self.scan.active = false;
        }
    }

    pub(crate) fn save_plugin_paths(&self) {
        let cache = PluginCache {
            saved_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            descriptors: self.plugins.clone(),
            vst3_paths: self.vst3_paths.clone(),
            disabled_vst3_paths: self.disabled_vst3_paths.clone(),
        };
        cache.save();
    }

    pub(crate) fn start_scan(&mut self) {
        if self.scan.active {
            return;
        }
        let Some(backend) = self.backend.clone() else {
            self.scan.error = Some("mic_host.dll is not loaded".to_string());
            return;
        };
        self.scan = ScanState {
            receiver: Some(plugin_cache::start(
                backend,
                self.vst3_paths.clone(),
                self.disabled_vst3_paths.clone(),
            )),
            active: true,
            done: 0,
            total: 0,
            error: None,
        };
        logging::line("scanning for VST3 plugins");
    }

    pub(crate) fn apply_chain_action(&mut self, action: ChainAction) {
        match action {
            ChainAction::Move(from, to) => {
                self.cancel_start_for_edit();
                if move_chain_entry(&mut self.project.chain, from, to) {
                    self.rebuild_chain();
                }
            }
            ChainAction::Remove(index) => {
                if index >= self.project.chain.len() {
                    return;
                }
                self.cancel_start_for_edit();
                let entry = self.project.chain.remove(index);
                self.project.remove_plugin_state(entry.id);
                self.project.prune_plugin_states();
                self.builtin_editors
                    .retain(|editor| editor.entry_id != entry.id);
                if let Some(session) = self.session.as_mut() {
                    session.retire_plugin(entry.id);
                    if let Err(error) = session.rebuild_engine(&self.project) {
                        logging::line(format!("retiring plugin {}: {error}", entry.id));
                        self.last_error = Some(error);
                        self.stop_audio();
                    }
                }
                self.mark_dirty();
                self.save_now();
            }
            ChainAction::Bypass(index, bypassed) => {
                self.cancel_start_for_edit();
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.bypassed = bypassed;
                    self.rebuild_chain();
                }
            }
            ChainAction::Oversampling(index, factor) => {
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.oversampling = factor;
                }
                self.settings_changed();
            }
            ChainAction::Sidechain(index, source) => {
                self.cancel_start_for_edit();
                if let Some(entry) = self.project.chain.get_mut(index) {
                    entry.sidechain_source = source;
                }
                self.settings_changed();
                self.save_now();
            }
            ChainAction::OpenEditor(index) => {
                let Some(entry) = self.project.chain.get(index) else {
                    return;
                };
                let Some(session) = self.session.as_ref() else {
                    self.last_error = Some("start audio to open plugin editors".to_string());
                    return;
                };
                if let Some((kind, params, analysis)) = session.builtin(entry.id) {
                    if !self
                        .builtin_editors
                        .iter()
                        .any(|editor| editor.entry_id == entry.id)
                    {
                        self.builtin_editors
                            .push(BuiltinEditor::new(entry.id, kind, params, analysis));
                    }
                    return;
                }
                if let Err(error) = session.open_editor(entry.id) {
                    self.last_error = Some(error);
                }
            }
            ChainAction::Duplicate(index) => self.begin_duplicate(index),
            ChainAction::Add(descriptor) => self.begin_add(&descriptor),
        }
    }

    pub(crate) fn allocate_plugin_id(&mut self) -> u64 {
        let id = self.next_plugin_id.max(1);
        self.next_plugin_id = id.saturating_add(1);
        id
    }

    pub(crate) fn cancel_start_for_edit(&mut self) {
        if self.session_worker.is_some() {
            self.stop_audio();
        }
    }

    pub(crate) fn rebuild_chain(&mut self) {
        self.measured = None;
        if let Some(session) = self.session.as_mut() {
            match session.rebuild_engine(&self.project) {
                Ok(()) => {}
                Err(error) => {
                    logging::line(format!("hot-swapping the chain: {error}"));
                    self.last_error = Some(error);
                    self.stop_audio();
                    return;
                }
            }
        }
        self.mark_dirty();
        self.save_now();
    }
}
