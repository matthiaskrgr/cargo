use std::cmp;
use std::env;
use std::time::{Duration, Instant};

use core::shell::Verbosity;
use util::{CargoResult, Config};

use unicode_width::UnicodeWidthChar;

pub struct Progress<'cfg> {
    state: Option<State<'cfg>>,
}

pub enum ProgressStyle {
    Percentage,
    Ratio,
}

struct State<'cfg> {
    config: &'cfg Config,
    format: Format,
    first: bool,
    last_update: Instant,
    name: String,
    done: bool,
}

struct Format {
    #[allow(dead_code)] // format used in tests
    format: String,
    max_width: usize,
    max_print: usize,
}

impl<'cfg> Progress<'cfg> {
    pub fn with_style(name: &str, format: &str, cfg: &'cfg Config) -> Progress<'cfg> {

        // report no progress when -q (for quiet) or TERM=dumb are set
        let dumb = match env::var("TERM") {
            Ok(term) => term == "dumb",
            Err(_) => false,
        };
        if cfg.shell().verbosity() == Verbosity::Quiet || dumb {
            return Progress { state: None };
        }

        let format = format.to_string();

        Progress {
            state: cfg.shell().err_width().map(|n| State {
                config: cfg,
                format: Format {
                    format,
                    max_width: n,
                    max_print: 80,
                },
                first: true,
                last_update: Instant::now(),
                name: name.to_string(),
                done: false,
            }),
        }
    }

    pub fn disable(&mut self) {
        self.state = None;
    }

    pub fn new(name: &str, cfg: &'cfg Config) -> Progress<'cfg> {
        Self::with_style(name, "[%b] %s/%t%n", cfg)
    }

    pub fn tick(&mut self, cur: usize, max: usize) -> CargoResult<()> {
        match self.state {
            Some(ref mut s) => s.tick(cur, max, 0, "", "", true),
            None => Ok(()),
        }
    }

    pub fn clear(&mut self) {
        if let Some(ref mut s) = self.state {
            s.clear();
        }
    }

    pub fn tick_now(&mut self, cur: usize, max: usize,format: &str, active_names: Vec<String>) -> CargoResult<()> {
        let active = active_names.len();
        let msg = &format!(": {}", active_names.join(", "));
        match self.state {
            Some(ref mut s) => s.tick(cur, max, active,format, msg, false),
            None => Ok(()),
        }
    }
}

impl<'cfg> State<'cfg> {
    fn tick(&mut self, cur: usize, max: usize, active: usize, format: &str,  msg: &str, throttle: bool) -> CargoResult<()> {
        if self.done {
            return Ok(());
        }

        // Don't update too often as it can cause excessive performance loss
        // just putting stuff onto the terminal. We also want to avoid
        // flickering by not drawing anything that goes away too quickly. As a
        // result we've got two branches here:
        //
        // 1. If we haven't drawn anything, we wait for a period of time to
        //    actually start drawing to the console. This ensures that
        //    short-lived operations don't flicker on the console. Currently
        //    there's a 500ms delay to when we first draw something.
        // 2. If we've drawn something, then we rate limit ourselves to only
        //    draw to the console every so often. Currently there's a 100ms
        //    delay between updates.
        if throttle {
            if self.first {
                let delay = Duration::from_millis(500);
                if self.last_update.elapsed() < delay {
                    return Ok(());
                }
                self.first = false;
            } else {
                let interval = Duration::from_millis(100);
                if self.last_update.elapsed() < interval {
                    return Ok(());
                }
            }
            self.last_update = Instant::now();
        }

        if cur == max {
            self.done = true;
        }

        // Write out a pretty header, then the progress bar itself, and then
        // return back to the beginning of the line for the next print.
        self.try_update_max_width();
        if let Some(string) = self.format.progress_status(cur, max, active, format,  msg) {
            self.config.shell().status_header(&self.name)?;
            write!(self.config.shell().err(), "{}\r", string)?;
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.try_update_max_width();
        let blank = " ".repeat(self.format.max_width);
        drop(write!(self.config.shell().err(), "{}\r", blank));
    }

    fn try_update_max_width(&mut self) {
        if let Some(n) = self.config.shell().err_width() {
            self.format.max_width = n;
        }
    }
}

impl Format {
    fn progress_status(&self, cur: usize, max: usize, _active: usize, format: &str,  msg: &str,) -> Option<String> {
        // we can use CARGO_STATUS env var to controll status bar format, inspired by Ninja
        // %b: progress bar
        // %s: started jobs
        // %t: total jobs we have to process to finish the build
        // %p: progress percentage
        // %n: job names (", "-seperated list)
        // %%: single % char

        let status_template = format.to_string();

        let mut template_bare = status_template.clone();
        //let template_original = template_bare.clone();
        // remove all the parameters so we get the bare skeleton
        // we need the length of this for formatting

        let  mut things_stripped = 0; // we need this to know how many params*2 (= chars) we got shorter
        for param in &["%b", "%s", "%t", "%n", "%P"] {
            template_bare = template_bare.replace(param, "");
            things_stripped += 1;
        }

        // Render the percentage at the far right and then figure how long the
        // progress bar is
        let pct = (cur as f64) / (max as f64);
        let pct = if !pct.is_finite() { 0.0 } else { pct };
        let percentage = format!(" {:6.02}%", pct * 100.0);
        let stats_len = cur.to_string().len() + max.to_string().len() + template_bare.len() /* remaining chars */;
        let extra_len = stats_len + 15 /* status header */;
        let display_width = match self.width().checked_sub(extra_len) {
            Some(n) => n,
            None => return None,
        };

        let mut status: String = status_template;
        status = status.replace("%s", &cur.to_string());
        status = status.replace("%t", &max.to_string());
        status = status.replace("%p", &percentage);

        //let mut string = String::with_capacity(self.max_width);

        let mut progress_bar = String::with_capacity(self.max_width);
        let hashes = display_width as f64 * pct;
        let hashes = hashes as usize;

        // Draw the `===>`
        if hashes > 0 {
            progress_bar.push_str(&"=".repeat(hashes-1));
            if cur == max {
                progress_bar.push_str("=");
            } else {
                progress_bar.push_str(">");
            }
        }

        // Draw the empty space we have left to do
        progress_bar.push_str(&" ".repeat(display_width - hashes));
        //string.push_str("]");
        //string.push_str(&stats); //   x/y   or   x %
        let mut active_jobs: String = String::new();
        let mut avail_msg_len = self.max_width  - (status.len() + stats_len + progress_bar.len() - things_stripped*2 ) /* we replace +b */ ; //- self.width();
        let mut ellipsis_pos = 0;
        if avail_msg_len > 3 {
            for c in msg.chars() {
                let display_width = c.width().unwrap_or(0);
                if avail_msg_len >= display_width {
                    avail_msg_len -= display_width;
                    active_jobs.push(c);
                    if avail_msg_len >= 3 {
                        ellipsis_pos = active_jobs.len();
                    }
                } else {
                    active_jobs.truncate(ellipsis_pos);
                    active_jobs.push_str("...");
                    break;
                }
            }
        }

        status = status.replace("%b", &progress_bar);

        status = status.replace("%n", &active_jobs);

        //Some(string)
        Some(status)
    }

    fn width(&self) -> usize {
        cmp::min(self.max_width, self.max_print)
    }
}

impl<'cfg> Drop for State<'cfg> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[test]
fn test_progress_status() {
    let format = Format {
        format: "[%b] %s/%t%n".to_string(),
        max_print: 40,
        max_width: 60,
    };
    assert_eq!(
        format.progress_status(0, 4, 1,&format.format, ""),
        Some("[                   ] 0/4".to_string())
    );
    assert_eq!(
        format.progress_status(1, 4, 1,&format.format, ""),
        Some("[===>               ] 1/4".to_string())
    );
    assert_eq!(
        format.progress_status(2, 4, 1, &format.format,""),
        Some("[========>          ] 2/4".to_string())
    );
    assert_eq!(
        format.progress_status(3, 4, 1,&format.format,""),
        Some("[=============>     ] 3/4".to_string())
    );
    assert_eq!(
        format.progress_status(4, 4, 1, &format.format,""),
        Some("[===================] 4/4".to_string())
    );

    assert_eq!(
        format.progress_status(3999, 4000, 1, &format.format,""),
        Some("[===========> ] 3999/4000".to_string())
    );
    assert_eq!(
        format.progress_status(4000, 4000, 0,&format.format, ""),
        Some("[=============] 4000/4000".to_string())
    );

    assert_eq!(
        format.progress_status(3, 4, 1, &format.format,": short message"),
        Some("[=============>     ] 3/4: short message".to_string())
    );
    assert_eq!(
        format.progress_status(3, 4, 1, &format.format,": msg thats just fit"),
        Some("[=============>     ] 3/4: msg thats just fit".to_string())
    );
    assert_eq!(
        format.progress_status(3, 4, 1, &format.format,": msg that's just fit"),
        Some("[=============>     ] 3/4: msg that's just...".to_string())
    );

    // combining diacritics have width zero and thus can fit max_width.
    let zalgo_msg = "z̸̧̢̗͉̝̦͍̱ͧͦͨ̑̅̌ͥ́͢a̢ͬͨ̽ͯ̅̑ͥ͋̏̑ͫ̄͢͏̫̝̪̤͎̱̣͍̭̞̙̱͙͍̘̭͚l̶̡̛̥̝̰̭̹̯̯̞̪͇̱̦͙͔̘̼͇͓̈ͨ͗ͧ̓͒ͦ̀̇ͣ̈ͭ͊͛̃̑͒̿̕͜g̸̷̢̩̻̻͚̠͓̞̥͐ͩ͌̑ͥ̊̽͋͐̐͌͛̐̇̑ͨ́ͅo͙̳̣͔̰̠̜͕͕̞̦̙̭̜̯̹̬̻̓͑ͦ͋̈̉͌̃ͯ̀̂͠ͅ ̸̡͎̦̲̖̤̺̜̮̱̰̥͔̯̅̏ͬ̂ͨ̋̃̽̈́̾̔̇ͣ̚͜͜h̡ͫ̐̅̿̍̀͜҉̛͇̭̹̰̠͙̞ẽ̶̙̹̳̖͉͎̦͂̋̓ͮ̔ͬ̐̀͂̌͑̒͆̚͜͠ ͓͓̟͍̮̬̝̝̰͓͎̼̻ͦ͐̾̔͒̃̓͟͟c̮̦͍̺͈͚̯͕̄̒͐̂͊̊͗͊ͤͣ̀͘̕͝͞o̶͍͚͍̣̮͌ͦ̽̑ͩ̅ͮ̐̽̏͗́͂̅ͪ͠m̷̧͖̻͔̥̪̭͉͉̤̻͖̩̤͖̘ͦ̂͌̆̂ͦ̒͊ͯͬ͊̉̌ͬ͝͡e̵̹̣͍̜̺̤̤̯̫̹̠̮͎͙̯͚̰̼͗͐̀̒͂̉̀̚͝͞s̵̲͍͙͖̪͓͓̺̱̭̩̣͖̣ͤͤ͂̎̈͗͆ͨͪ̆̈͗͝͠";
    assert_eq!(
        format.progress_status(3, 4, 1, &format.format,zalgo_msg),
        Some("[=============>     ] 3/4".to_string() + zalgo_msg)
    );

    // some non-ASCII ellipsize test
    assert_eq!(
        format.progress_status(3, 4, 1, &format.format, "_123456789123456e\u{301}\u{301}8\u{301}90a"),
        Some("[=============>     ] 3/4_123456789123456e\u{301}\u{301}...".to_string())
    );
    assert_eq!(
        format.progress_status(3, 4, 1,&format.format,"：每個漢字佔據了兩個字元"),
        Some("[=============>     ] 3/4：每個漢字佔據了...".to_string())
    );
}

#[test]
fn test_progress_status_percentage() {
    let format = Format {
        format: "[%b] %p%n".to_string(),
        max_print: 40,
        max_width: 60,
    };
    assert_eq!(
        format.progress_status(0, 77, 1,  &format.format,""),
        Some("[               ]   0.00%".to_string())
    );
    assert_eq!(
        format.progress_status(1, 77, 1, &format.format,""),
        Some("[               ]   1.30%".to_string())
    );
    assert_eq!(
        format.progress_status(76, 77, 1, &format.format,""),
        Some("[=============> ]  98.70%".to_string())
    );
    assert_eq!(
        format.progress_status(77, 77, 1,&format.format, ""),
        Some("[===============] 100.00%".to_string())
    );
}

#[test]
fn test_progress_status_too_short() {
    let format = Format {
        format: "[%b] %p%n".to_string(),
        max_print: 25,
        max_width: 25,
    };
    assert_eq!(
        format.progress_status(1, 1, 0, &format.format, ""),
        Some("[] 100.00%".to_string())
    );

    let format = Format {
        format: "[%b] %p%n".to_string(),
        max_print: 24,
        max_width: 24,
    };
    assert_eq!(
        format.progress_status(1, 1, 0, &format.format, ""),
        None
    );
}
