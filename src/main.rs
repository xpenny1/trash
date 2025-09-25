use nix::sys::signal::{SigHandler, Signal, signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{
    ForkResult, Pid, chdir, execvp, fork, getcwd, getpid, setpgid, tcsetpgrp, write,
};
use std::env;
use std::ffi::CString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::exit;
use std::ops;

enum Command {
    Builtin(BuiltinCommand),
    External(ExternalCommand),
}

enum BuiltinCommand {
    Exit,
    Cd(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
enum Quoting {
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

#[derive(Debug, PartialEq, Eq)]
enum Operator {
    And,
    Or,
    Pipe,
    Andpercent,
    Semicolon,
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String, Quoting),
    Operator(Operator),
    Whitespace,
}


type ParseResult<'a, Output> = Option<(Output, &'a str)>;

trait Parser<'a, Output> {
    fn parse(&self, input: &'a str) -> ParseResult<'a, Output>;
    fn many(&self) -> impl Parser<'a, Vec<Output>> {
        move |input: &'a str| -> ParseResult<'a,Vec<Output>> {
            let mut remaining: &'a str = "";
            let mut parsed: Vec<Output> = Vec::new();
            loop {
                if let Some((p,r)) = self.parse(input) {
                    parsed.push(p);
                    remaining = r;
                } else {
                    return Option::Some((parsed, remaining))
                }
            }
        }
    }
    fn or<P> (&self, parser: P) -> impl Parser<'a, Output>
        where
            P: Parser<'a, Output>
    {
        move |input| {
            if let Option::Some(result) = self.parse(input) {
                Option::Some(result)
            } else {
                parser.parse(input)
            }
        }
    }
    fn drop_and_then<NewOutput, P> (&self, parser: P) -> impl Parser<'a, NewOutput>
    where
        P: Parser<'a, NewOutput>
    {
        move |input| {
            let (_parsed, remaining) = self.parse(input)?;
            parser.parse(remaining)
        }
    }
    fn and_then_drop<NewOutput, P> (&self, parser: P) -> impl Parser<'a, Output>
    where
        P: Parser<'a, NewOutput>
    {
        move |input: &'a str| -> Option<(Output, &'a str)> {
            let (parsed1, remaining1) = self.parse(input)?;
            let (_parsed2, remaining2) = parser.parse(remaining1)?;
            Option::Some((parsed1, remaining2))
        }
    }
    fn pair<SecondOutput, P> (&self, parser: P) -> impl Parser<'a, (Output, SecondOutput)>
    where
        P: Parser<'a, SecondOutput>
    {
        move |input| {
            let (first, remaining1) = self.parse(input)?;
            let (second, remaining2) = parser.parse(remaining1)?;
            Option::Some(((first,second), remaining2))
        }
    }
    fn map<NewOutput, F> (&self, mapping_function: F) -> impl Parser<'a, NewOutput>
    where
        F: Fn(Output) -> NewOutput
    {
        move |input| {
            let (parsed, remaining) = self.parse(input)?;
            Option::Some((mapping_function(parsed), remaining))
        }
    }
}
impl<'a, F, Output> Parser<'a, Output> for F
where
    F: Fn(&'a str) -> ParseResult<'a, Output>
{
    fn parse(&self, input: &'a str) -> ParseResult<'a, Output> {
        self(input)
    }
}

fn string_parser<'a>(expected: &'static str) -> impl Parser<'a, &'static str> {
    move |input: &'a str| -> Option<(&'static str, &'a str)> {
        if let Option::Some(suffix) = input.strip_prefix(expected) {
            Option::Some((expected, suffix))
        } else {
            Option::None
        }
    }     
}
fn never_parser<'a, Output>() -> impl Parser<'a, Output> {
    |_inpupt| Option::None
}

fn test_parser() {
    let input: &str = "Hallo Welt";
    let parser = string_parser("Hallo");
    let space_char = string_parser(" ");
    let tab_char = string_parser("\t");
    let newline_char = string_parser("\n");
    let space_ = space_char.or(tab_char);
    let space = space_.or(newline_char);
    let whitspaces = space.many();
    print!("Parsed: {}", whitspaces.parse(parser.parse(input).unwrap().1).unwrap().1)
}

struct Shell {
    shell_pid: Pid,
    last_status: i32,
    stdin_handle: std::io::Stdin,
    stdout_handle: std::io::Stdout,
    // TODO: jobs table
}

impl Shell {
    fn new() -> nix::Result<Self> {
        // ignore signals
        unsafe {
            // required when shell process is not foreground and uses tcsetpgrp
            signal(Signal::SIGTTOU, SigHandler::SigIgn)?;
            // required for ignoring ctrl-z
            signal(Signal::SIGTSTP, SigHandler::SigIgn)?;
        }
        let shell_pid = getpid();
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        setpgid(shell_pid, shell_pid)?;
        tcsetpgrp(&stdin, shell_pid)?;

        Ok(Self {
            last_status: 0,
            shell_pid: shell_pid,
            stdin_handle: stdin,
            stdout_handle: stdout,
        })
    }

    fn run(&mut self) -> nix::Result<()> {
        loop {
            print!("\n$ ");
            self.stdout_handle.flush().unwrap();

            let mut input = String::new();
            let bytes_read = self.stdin_handle.lock().read_line(&mut input).unwrap();

            if bytes_read == 0 {
                println!("\nexit");
                exit(0);
            }
            let alpha_parser = {
                let a_parser = string_parser("a");
                let ab_parser = a_parser.or(string_parser("b"));
                let abc_parser = ab_parser.or(string_parser("c"));
                let abcd_parser = abc_parser.or(string_parser("d"));
                let abcde_parser = abcd_parser.or(string_parser("e"));
                let abcdef_parser = abcde_parser.or(string_parser("f"));
                let abcdefg_parser = abcdef_parser.or(string_parser("g"));
                let abcdefgh_parser = abcdefg_parser.or(string_parser("h"));
                let abcdefghi_parser = abcdefgh_parser.or(string_parser("i"));
                let abcdefghij_parser = abcdefghi_parser.or(string_parser("j"));
                let abcdefghijk_parser = abcdefghij_parser.or(string_parser("k"));
                let abcdefghijkl_parser = abcdefghijk_parser.or(string_parser("l"));
                let abcdefghijklm_parser = abcdefghijkl_parser.or(string_parser("m"));
                let abcdefghijklmn_parser = abcdefghijklm_parser.or(string_parser("n"));
                let abcdefghijklmno_parser = abcdefghijklmn_parser.or(string_parser("o"));
                let abcdefghijklmnop_parser = abcdefghijklmno_parser.or(string_parser("p"));
                let abcdefghijklmnopq_parser = abcdefghijklmnop_parser.or(string_parser("q"));
                let abcdefghijklmnopqr_parser = abcdefghijklmnopq_parser.or(string_parser("r"));
                let abcdefghijklmnopqrs_parser = abcdefghijklmnopqr_parser.or(string_parser("s"));
                let abcdefghijklmnopqrst_parser = abcdefghijklmnopqrs_parser.or(string_parser("t"));
                let abcdefghijklmnopqrstu_parser = abcdefghijklmnopqrst_parser.or(string_parser("u"));
                let abcdefghijklmnopqrstuv_parser = abcdefghijklmnopqrstu_parser.or(string_parser("v"));
                let abcdefghijklmnopqrstuvw_parser = abcdefghijklmnopqrstuv_parser.or(string_parser("w"));
                let abcdefghijklmnopqrstuvwx_parser = abcdefghijklmnopqrstuvw_parser.or(string_parser("x"));
                let abcdefghijklmnopqrstuvwxy_parser = abcdefghijklmnopqrstuvwx_parser.or(string_parser("y"));
                let abcdefghijklmnopqrstuvwxyz_parser = abcdefghijklmnopqrstuvwxy_parser.or(string_parser("z"));               
                abcdefghijklmnopqrstuvwxyz_parser
            }
            let space_char = string_parser(" ");
            let tab_char = string_parser("\t");
            let newline_char = string_parser("\n");
            let space_ = space_char.or(tab_char);
            let space = space_.or(newline_char);
            let whitspaces = space.many();
            let word_parser = whitspaces.drop_and_then(alpha_parser.many()); 
            let external_command_parser = word_parser.pair(word_parser.many()).and_then_drop(whitspaces);
            let cd_parser = whitspaces
                .drop_and_then(string_parser("cd"))
                .pair(word_parser.many())
                .map(|tup: (&'static str, Vec<&str>)| -> Command {
                    Command::Builtin(
                        BuiltinCommand::Cd(
                            vec![]
//                            tup.1.iter().map(|arg| arg.to_string()).collect()
                        )
                    )
                });
            let exit_parser = whitspaces.drop_and_then(string_parser("exit"));
            if let Option::Some((command, args)) = command_parser.parse(&input) {
                self.execute(command)?;
            }
//            let tokens = parser.tokenize(input.as_str());
//
//            if let Some(command) = parser.parse(tokens) {
//                self.execute(command)?;
//            }
        }
    }

    fn execute(&mut self, command: Command) -> nix::Result<()> {
        match command {
            Command::Builtin(builtin) => {
                let _ = self.handle_builtin(builtin);
                Ok(())
            }
            Command::External(external) => {
                let status = self.spawn_foreground(external)?;
                if let WaitStatus::Exited(_, code) = status {
                    self.last_status = code;
                } else if let WaitStatus::Stopped(child_pid, signal) = status {
                    // TODO: add to job table
                    println!("\n{} suspended", child_pid);
                }
                Ok(())
            }
        }
    }

    fn spawn_foreground(&self, command: ExternalCommand) -> nix::Result<WaitStatus> {
        match unsafe { fork() } {
            Ok(ForkResult::Parent { child, .. }) => {
                let _ = setpgid(child, child);
                let _ = tcsetpgrp(&std::io::stdin(), child);
                // maybe WUNTRACED/WCONTINUED later for ctrl-z job controll
                let status = waitpid(child, Some(WaitPidFlag::WUNTRACED))?;
                let _ = tcsetpgrp(&std::io::stdin(), self.shell_pid);
                Ok(status)
            }
            Ok(ForkResult::Child) => {
                // reset signal handlers
                unsafe {
                    signal(Signal::SIGTSTP, SigHandler::SigDfl)?;
                    signal(Signal::SIGTTOU, SigHandler::SigDfl)?;
                }
                let _ = setpgid(Pid::from_raw(0), Pid::from_raw(0));
                let _ = execvp(&command.cmd_as_cstring(), &command.args_as_cstring());
                write(std::io::stdout(), b"command not found\n").ok();
                unsafe { libc::_exit(127) };
            }
            Err(_) => {
                println!("Fork failed");
                Err(nix::Error::EINVAL)
            }
        }
    }

    fn handle_builtin(&self, builtin: BuiltinCommand) -> nix::Result<()> {
        match builtin {
            BuiltinCommand::Exit => {
                println!("exit");
                exit(0);
            }
            BuiltinCommand::Cd(args) => {
                let target = match &args[1..] {
                    [] => {
                        match env::var("HOME") {
                            Ok(home) => PathBuf::from(home),
                            Err(_) => {
                                eprintln!("cd: HOME is not set");
                                return Err(nix::Error::EINVAL);
                            }
                        }
                    }
                    [_, dir] if dir == "-" => {
                        match env::var("OLDPWD") {
                            Ok(oldpwd) => {
                                println!("{}", oldpwd);
                                PathBuf::from(oldpwd)
                            }
                            Err(_) => {
                                eprintln!("cd: OLDPWD is not set");
                                return Err(nix::Error::EINVAL);
                            }
                        }
                    }
                    [_, dir] => PathBuf::from(dir),
                    _ => {
                        eprintln!("cd: too many arguments");
                        return Err(nix::Error::EINVAL);
                    }
                };

                let pwd = getcwd()?;

                if let Err(e) = chdir(&target) {
                    eprintln!("cd: {}", e);
                } else {
                    // update PWD and OLDPWD
                    unsafe {
                        let new_pwd = getcwd()?;
                        env::set_var("OLDPWD", pwd.as_os_str());
                        env::set_var("PWD", new_pwd.as_os_str());
                    };
                }
            }
        }

        Ok(())
    }
}

struct ExternalCommand {
    // TODO: look into OsString for POSIX compatibility
    cmd: String,
    args: Vec<String>,
    // redirects: Vec<Redirect>,
    // background: bool,
}

impl ExternalCommand {
    fn new(cmd: String, args: Vec<String>) -> Self {
        Self { cmd, args }
    }

    fn cmd_as_cstring(&self) -> CString {
        CString::new(self.cmd.as_str()).unwrap()
    }

    fn args_as_cstring(&self) -> Vec<CString> {
        self.args
            .iter()
            .map(|arg| CString::new(arg.as_str()).unwrap())
            .collect()
    }
}

fn main() {
    let mut shell = Shell::new().expect("Failed to spawn shell");
    shell.run().expect("Failed to run shell");
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_simple_words() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo hello world");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hello".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("world".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_simple_single_quotes() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo 'hello world'");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hello world".into(), Quoting::SingleQuoted),
            ]
        );
    }

    #[test]
    fn test_simple_double_quotes() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo \"hello world\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hello world".into(), Quoting::DoubleQuoted),
            ]
        );
    }

    #[test]
    fn test_single_and_double_quotes_with_space() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo 'hello' \"world\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hello".into(), Quoting::SingleQuoted),
                Token::Whitespace,
                Token::Word("world".into(), Quoting::DoubleQuoted),
            ]
        );
    }

    #[test]
    fn test_single_and_double_quotes_without_space() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo 'hello'\"world\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hello".into(), Quoting::SingleQuoted),
                Token::Word("world".into(), Quoting::DoubleQuoted),
            ]
        );
    }

    #[test]
    fn test_single_and_double_inside_eachother() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo 'he\"llo' \"w'orld\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("he\"llo".into(), Quoting::SingleQuoted),
                Token::Whitespace,
                Token::Word("w'orld".into(), Quoting::DoubleQuoted),
            ]
        );
    }

    #[test]
    fn test_simple_semicolon() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo hi;");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hi".into(), Quoting::Unquoted),
                Token::Operator(Operator::Semicolon),
            ]
        );
    }

    #[test]
    fn test_simple_and() {
        let parser = Parser::new();
        let tokens = parser.tokenize("hello && world");
        assert_eq!(
            tokens,
            vec![
                Token::Word("hello".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Operator(Operator::And),
                Token::Whitespace,
                Token::Word("world".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_simple_andpercent() {
        let parser = Parser::new();
        let tokens = parser.tokenize("ls &");
        assert_eq!(
            tokens,
            vec![
                Token::Word("ls".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Operator(Operator::Andpercent),
            ]
        );
    }

    #[test]
    fn test_simple_pipe() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo 'hi' | cat");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hi".into(), Quoting::SingleQuoted),
                Token::Whitespace,
                Token::Operator(Operator::Pipe),
                Token::Whitespace,
                Token::Word("cat".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_simple_or() {
        let parser = Parser::new();
        let tokens = parser.tokenize("no || yes");
        assert_eq!(
            tokens,
            vec![
                Token::Word("no".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Operator(Operator::Or),
                Token::Whitespace,
                Token::Word("yes".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_multiple_whitespace() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo  hi");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("hi".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_escaping_double_quotes() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo \\\"hi\\\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("\"hi\"".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_escaping_backslash() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo \\\\");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("\\".into(), Quoting::Unquoted),
            ]
        );
    }

    #[test]
    fn test_escaping_newline() {
        let parser = Parser::new();
        let tokens = parser.tokenize("echo \"\\n\"");
        assert_eq!(
            tokens,
            vec![
                Token::Word("echo".into(), Quoting::Unquoted),
                Token::Whitespace,
                Token::Word("\n".into(), Quoting::DoubleQuoted),
            ]
        );
    }
}
