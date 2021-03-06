//! This documentation for the Sniprun project
//!
//! Sniprun is a neovim plugin that run parts of code.

use dirs::cache_dir;
use log::{info, LevelFilter};
use neovim_lib::{Neovim, NeovimApi, Session, Value};
use simple_logging::log_to_file;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

mod error;
mod interpreter;
mod interpreters;
mod launcher;

///This struct holds (with ownership) the data Sniprun and neovim
///give to the interpreter.
///This should be enough to implement up to project-level interpreters.
#[derive(Debug, Clone, PartialEq)]
pub struct DataHolder {
    /// contains the filetype of the file as return by `:set ft?`
    filetype: String,
    ///This contains the current line of code from where the user ran sniprun, and
    ///want to execute
    current_line: String,
    ///This contains the current block of text, if the user selected a bloc of code and ran snirpun
    ///on it
    current_bloc: String,
    ///The inclusive limits of the selected block (line numbers)
    range: [i64; 2],
    /// path of the current file that's being edited
    filepath: String,
    /// Field is left blank as of v0.3
    projectroot: String,
    /// field is left blank as of v0.3
    dependencies_path: Vec<String>,
    /// path to the cache directory that sniprun create
    work_dir: String,
    /// path to sniprun root, eg in case you need ressoruces from the ressources folder
    sniprun_root_dir: String,
}

impl DataHolder {
    ///create a new but almost empty DataHolder
    fn new() -> Self {
        std::fs::create_dir_all(format!(
            "{}/{}",
            cache_dir().unwrap().to_str().unwrap(),
            "sniprun"
        ))
        .unwrap();

        DataHolder {
            filetype: String::from(""),
            current_line: String::from(""),
            current_bloc: String::from(""),
            range: [-1, -1],
            filepath: String::from(""),
            projectroot: String::from(""),
            dependencies_path: vec![],
            work_dir: format!("{}/{}", cache_dir().unwrap().to_str().unwrap(), "sniprun"),
            sniprun_root_dir: String::from(""),
        }
    }
    ///remove and recreate the cache directory (is invoked by `:SnipReset`)
    fn clean_dir(&mut self) {
        let work_dir_path = self.work_dir.clone();
        std::fs::remove_dir_all(&work_dir_path).unwrap();
        std::fs::create_dir_all(&work_dir_path).unwrap();
    }
}

struct EventHandler {
    nvim: Neovim,
    data: DataHolder,
}

enum Messages {
    Run,
    Clean,
    Unknown(String),
}

impl From<String> for Messages {
    fn from(event: String) -> Self {
        match &event[..] {
            "run" => Messages::Run,
            "clean" => Messages::Clean,
            _ => Messages::Unknown(event),
        }
    }
}

impl EventHandler {
    fn new() -> EventHandler {
        let session = Session::new_parent().unwrap();
        let nvim = Neovim::new(session);
        let data = DataHolder::new();
        EventHandler { nvim, data }
    }

    /// fill the DataHolder with data from sniprun and Neovim
    fn fill_data(&mut self, values: Vec<Value>) {
        self.data.range = [values[0].as_i64().unwrap(), values[1].as_i64().unwrap()];
        self.data.sniprun_root_dir = String::from(values[2].as_str().unwrap());

        //get filetype
        let ft = self.nvim.command_output("set ft?");
        if let Ok(real_ft) = ft {
            self.data.filetype = String::from(real_ft.split("=").last().unwrap());
        }

        //get current line
        let current_line = self.nvim.get_current_line();
        if let Ok(real_current_line) = current_line {
            self.data.current_line = real_current_line;
        }

        //get current bloc
        let current_bloc = self.nvim.get_current_buf().unwrap().get_lines(
            &mut self.nvim,
            self.data.range[0] - 1, //because the function is 0-based instead of 1 and end-exclusive
            self.data.range[1],
            false,
        );
        if let Ok(real_current_bloc) = current_bloc {
            self.data.current_bloc = real_current_bloc.join("\n");
        }

        //get full file path
        let full_file_path = self.nvim.command_output("echo expand('%:p')");
        if let Ok(real_full_file_path) = full_file_path {
            self.data.filepath = real_full_file_path;
        }
    }
}
enum HandleAction {
    New(thread::JoinHandle<()>),
}

fn main() {
    let mut event_handler = EventHandler::new();
    let _ = log_to_file(
        &format!("{}/{}", event_handler.data.work_dir, "sniprun.log"),
        LevelFilter::Info,
    );

    info!("[MAIN] SnipRun launched successfully");

    let receiver = event_handler.nvim.session.start_event_loop_channel();
    let meh = Arc::new(Mutex::new(event_handler));

    let (send, recv) = mpsc::channel();
    thread::spawn(move || {
        let mut _handle: Option<thread::JoinHandle<()>> = None;
        loop {
            match recv.recv() {
                Err(_) => panic!("Broken connection"),
                Ok(HandleAction::New(new)) => _handle = Some(new),
            }
        }
    });

    //main loop
    info!("[MAIN] Start of main event loop");
    for (event, values) in receiver {
        match Messages::from(event.clone()) {
            //Run command
            Messages::Run => {
                info!("[MAINLOOP] Run command received");

                let cloned_meh = meh.clone();
                let _res2 = send.send(HandleAction::New(thread::spawn(move || {
                    // get up-to-date data
                    //
                    cloned_meh.lock().unwrap().fill_data(values);

                    //run the launcher (that selects, init and run an interpreter)
                    let launcher = launcher::Launcher::new(cloned_meh.lock().unwrap().data.clone());
                    let result = launcher.select_and_run();
                    info!("[MAINLOOP] Interpreter return a result");

                    // return Ok(result) or Err(sniprunerror)
                    match result {
                        Ok(answer_str) => {
                            let mut answer_str = answer_str.clone();
                            answer_str = answer_str.replace("\\\"", "\"");
                            answer_str = answer_str.replace("\"", "\\\"");
                            //make sure there is no lone "
                            let len_without_newline = answer_str.trim_end().len();
                            answer_str.truncate(len_without_newline);

                            info!("[MAINLOOP] Returning stdout of code run: {}", answer_str);

                            let _ = cloned_meh
                                .lock()
                                .unwrap()
                                .nvim
                                .command(&format!("echo \"{}\"", answer_str));
                        }
                        Err(e) => {
                            info!("[MAINLOOP] Returning an error");
                            let _ = cloned_meh
                                .lock()
                                .unwrap()
                                .nvim
                                .err_writeln(&format!("{}", e));
                        }
                    };

                    //display ouput in nvim

                    //clean data
                    cloned_meh.lock().unwrap().data = DataHolder::new();
                })));
            }
            Messages::Clean => {
                info!("[MAINLOOP] Clean command received");
                meh.clone().lock().unwrap().data.clean_dir()
            }

            Messages::Unknown(event) => {
                info!("[MAINLOOP] Unknown event received: {:?}", event);
            }
        }
    }
}
