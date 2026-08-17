//! Background work used by the modeless VFX Builder.

use eframe::egui;
use engine_authoring::{
    replace_file_contents, VfxAuthoringService, VfxCompilation, VfxEffect,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

struct VfxCompileJob {
    effect: VfxEffect,
    receiver: Receiver<VfxCompilation>,
}

pub(super) enum VfxIoCompletion {
    Open { path: PathBuf, result: Result<VfxEffect, String> },
    Save { result: Result<(), String> },
    Create { path: PathBuf, effect: VfxEffect, result: Result<(), String> },
}

#[derive(Default)]
pub(super) struct VfxBackgroundTasks {
    compiled_effect: Option<VfxEffect>,
    compilation: Option<VfxCompilation>,
    compile_job: Option<VfxCompileJob>,
    io_job: Option<Receiver<VfxIoCompletion>>,
}

impl VfxBackgroundTasks {
    pub(super) fn compilation_for(
        &mut self,
        effect: &VfxEffect,
        ctx: &egui::Context,
    ) -> Option<VfxCompilation> {
        self.poll_compile(effect);
        if self.compile_job.is_none() && self.compiled_effect.as_ref() != Some(effect) {
            let requested = effect.clone();
            let worker_effect = requested.clone();
            let repaint = ctx.clone();
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                let compilation = VfxAuthoringService::new().compile(&worker_effect);
                let _ = sender.send(compilation);
                repaint.request_repaint();
            });
            self.compile_job = Some(VfxCompileJob { effect: requested, receiver });
        }
        if self.compiled_effect.as_ref() == Some(effect) {
            self.compilation.clone()
        } else {
            None
        }
    }

    fn poll_compile(&mut self, current_effect: &VfxEffect) {
        let Some(job) = self.compile_job.as_ref() else { return; };
        match job.receiver.try_recv() {
            Ok(compilation) => {
                let compiled_effect = job.effect.clone();
                self.compile_job = None;
                if &compiled_effect == current_effect {
                    self.compiled_effect = Some(compiled_effect);
                    self.compilation = Some(compilation);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.compile_job = None,
        }
    }

    pub(super) fn io_busy(&self) -> bool { self.io_job.is_some() }

    pub(super) fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.io_busy() { return; }
        let repaint = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|json| VfxAuthoringService::new().effect_from_json(&json).map_err(|error| error.to_string()));
            let _ = sender.send(VfxIoCompletion::Open { path, result });
            repaint.request_repaint();
        });
        self.io_job = Some(receiver);
    }

    pub(super) fn save(&mut self, path: PathBuf, json: String, ctx: &egui::Context) {
        if self.io_busy() { return; }
        let repaint = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = replace_file_contents(&path, &json).map_err(|error| error.to_string());
            let _ = sender.send(VfxIoCompletion::Save { result });
            repaint.request_repaint();
        });
        self.io_job = Some(receiver);
    }

    pub(super) fn create(
        &mut self,
        path: PathBuf,
        effect: VfxEffect,
        json: String,
        ctx: &egui::Context,
    ) {
        if self.io_busy() { return; }
        let repaint = ctx.clone();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = replace_file_contents(&path, &json).map_err(|error| error.to_string());
            let _ = sender.send(VfxIoCompletion::Create { path, effect, result });
            repaint.request_repaint();
        });
        self.io_job = Some(receiver);
    }

    pub(super) fn take_io_completion(&mut self) -> Option<VfxIoCompletion> {
        let receiver = self.io_job.as_ref()?;
        match receiver.try_recv() {
            Ok(completion) => { self.io_job = None; Some(completion) }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.io_job = None;
                Some(VfxIoCompletion::Save {
                    result: Err("VFX background I/O worker disconnected.".to_owned()),
                })
            }
        }
    }
}
