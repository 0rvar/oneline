use regex::Regex;
use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use termion::{clear, terminal_size};

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();

    // Parse arguments
    let mut label = String::new();
    let mut command_pos = 1;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--label" => {
                if i + 1 < args.len() {
                    label = args[i + 1].clone();
                    i += 2;
                    command_pos += 2;
                } else {
                    eprintln!("Error: --label requires a value");
                    std::process::exit(1);
                }
            }
            x => {
                if x.starts_with("-") {
                    eprintln!("Error: Unknown option: {}", x);
                    std::process::exit(1);
                } else {
                    break; // Exit loop on first non-flag argument
                }
            }
        }
    }

    // Check if we have enough arguments for a command
    if command_pos >= args.len() {
        eprintln!("Usage: {} [--label \"Label\"] command [args...]", args[0]);
        eprintln!("Example: {} --label \"Building Project\" make all", args[0]);
        std::process::exit(1);
    }

    // If label wasn't set with --label, use up to the first 3 command parts
    if label.is_empty() {
        label = args[command_pos..]
            .iter()
            .take_while(|s| !s.starts_with("-"))
            .map(|s| s.trim())
            .collect::<Vec<_>>()
            .join(" ");
        if label.chars().count() > 32 {
            label = format!("{}…", &label.chars().take(32).collect::<String>());
        }
    }

    let command_name = &args[command_pos];
    let command_args = &args[(command_pos + 1)..];

    // Store stdout and stderr content in memory
    let stdout_content = Arc::new(Mutex::new(Vec::<String>::new()));
    let stderr_content = Arc::new(Mutex::new(Vec::<String>::new()));

    // Get terminal width
    let (term_width, _) = terminal_size().unwrap_or((80, 24));

    // Set environment variables for forcing color output
    let mut command = Command::new(command_name);
    command
        .args(command_args)
        .env("TERM", "xterm-256color")
        .env("FORCE_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            match e.downcast::<std::io::Error>() {
                Ok(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        eprintln!("Command not found: {command_name}");
                    } else {
                        eprintln!("Error: {}", e);
                    }
                }
                Err(e) => {
                    eprintln!("Error: Failed to start command: {e}");
                }
            }
            std::process::exit(1);
        }
    };

    // Set up pipes for stdout and stderr
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Failed to capture stdout"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Failed to capture stderr"))?;

    // Clone Arc references for threads
    let stdout_content_clone = Arc::clone(&stdout_content);
    let stderr_content_clone = Arc::clone(&stderr_content);

    // Create regex for stripping ANSI escape sequences
    let ansi_regex = Regex::new(
        r"\x1B(?:\][0-9;]*(?:;|;{2}).*?(?:\x07|\x1B\\)|[\[0-9;]*[a-zA-Z])|\x07|\xe2\x80\xa6",
    )
    .unwrap();
    let line_modifying_regex = Regex::new(r"(\r)|(\x1b\[K)|(\x1b\[1K)|(\x1b\[2K)|(\x1b\[[0-9]*G)|(\x1b\[[0-9]*C)|(\x1b\[[0-9]*D)|(\x1b\[s)|(\x1b\[u)|(\b)").unwrap();
    let prefix = format!("[{}] ", label);

    // Create a channel for interleaved output
    let (tx_stdout, rx) = mpsc::channel();
    let tx_stderr = tx_stdout.clone();

    // Thread for processing stdout
    let stdout_thread = thread::spawn(move || {
        let reader = BufReader::new(stdout_pipe);
        for line in reader.lines().map_while(Result::ok) {
            // Store the line
            if let Ok(mut content) = stdout_content_clone.lock() {
                content.push(line.clone());
            }

            // Send to channel for display
            let _ = tx_stdout.send(("stdout".to_string(), line));
        }
    });

    // Thread for capturing stderr
    let stderr_thread = thread::spawn(move || {
        let reader = BufReader::new(stderr_pipe);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(mut content) = stderr_content_clone.lock() {
                content.push(line.clone());
            }

            // Send to channel for display if --stderr is enabled
            let _ = tx_stderr.send(("stderr".to_string(), line));
        }
    });

    // Thread for displaying output from both streams
    let display_thread = thread::spawn(move || {
        let mut printed_anything = false;

        // Process messages from both stdout and stderr
        while let Ok((_, line)) = rx.recv() {
            // Skip empty lines
            if line.is_empty() {
                continue;
            }

            // Process and display the line
            process_output_line(
                &prefix,
                &line,
                &ansi_regex,
                &line_modifying_regex,
                term_width,
            );
            printed_anything = true;
        }

        printed_anything
    });

    // Wait for child process to complete
    let status = child.wait()?;

    // Wait for threads to finish
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    let printed = display_thread.join().unwrap_or(false);

    if printed {
        // Print a newline if something was printed
        println!();
    }

    if status.success() {
        return Ok(());
    }

    eprintln!(
        "Error: Command failed with exit code {}",
        status.code().unwrap_or(-1)
    );
    eprintln!("Error output:");

    // Print stderr content
    let mut printed_stderr = false;
    if let Ok(content) = stderr_content.lock() {
        for line in content.iter() {
            eprintln!("{}", line);
            printed_stderr = true;
        }
    }
    if !printed_stderr {
        if let Ok(content) = stdout_content.lock() {
            for line in content.iter() {
                eprintln!("{}", line);
            }
        }
    }

    std::process::exit(status.code().unwrap_or(1));
}

// Process and display a single line of output
fn process_output_line(
    prefix: &str,
    line: &str,
    ansi_regex: &Regex,
    line_modifying_regex: &Regex,
    term_width: u16,
) {
    // Skip empty or duplicate lines
    if line.is_empty() {
        return;
    }
    let prefix_len = prefix.len();

    // Replace problematic sequences
    let line = line_modifying_regex.replace_all(line, "").to_string();

    // Get clean version for length checking
    let clean_line = ansi_regex.replace_all(&line, "").to_string();
    let available_width = term_width as usize - prefix_len;

    // Truncate if needed
    let display_line = if clean_line.len() > available_width {
        truncate_with_ansi(&line, available_width)
    } else {
        line.clone()
    };

    // Clear line and print with prefix
    print!("\r{}", clear::CurrentLine);
    print!("{prefix}{display_line}");
    let _ = io::stdout().flush();
}

// Function to truncate string with ANSI escape sequences
fn truncate_with_ansi(input: &str, max_len: usize) -> String {
    let mut truncated = String::new();
    let mut visible_char_count = 0;
    let chars = input.char_indices();
    let mut in_ansi_sequence = false;

    for (_byte_pos, ch) in chars {
        // Handle ANSI escape sequence
        if ch == '\u{001B}' {
            // ESC character
            in_ansi_sequence = true;
            truncated.push(ch);
            continue;
        }

        if in_ansi_sequence {
            truncated.push(ch);
            // End of ANSI sequence detection
            if ch.is_ascii_uppercase() || ch.is_ascii_lowercase() {
                in_ansi_sequence = false;
            }
            continue;
        }

        // Regular character (not in an ANSI sequence)
        if visible_char_count < max_len {
            truncated.push(ch);
            visible_char_count += 1;
        } else {
            // We've reached the maximum visible length, add ellipsis and stop
            truncated.pop();
            truncated.push('…');
            break;
        }
    }

    truncated
}
