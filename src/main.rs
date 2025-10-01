use nix::sys::signal::{SigHandler, Signal, signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{
    ForkResult, Pid, chdir, execvp, fork, getcwd, getpid, setpgid, tcsetpgrp, write,
};
use regex::Regex;
use std::marker::PhantomData;
use std::rc::Rc;
use std::{env, ops};
use std::ffi::CString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::exit;

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

struct Parser {
    variable_regex: Regex,
}

impl Parser {
    fn new() -> Self {
        let variable_regex = Regex::new(r"\$([a-zA-Z0-9_]+|\$|!)").unwrap();
        Self { variable_regex }
    }

    fn tokenize(&self, input: &str) -> Vec<Token> {
        let mut single_quotes = false;
        let mut double_quotes = false;
        let mut chars = input.chars().peekable();
        let mut tokens: Vec<Token> = Vec::new();
        let mut current = String::new();

        while let Some(current_char) = chars.next() {
            // remove preceding whitespace
            if !single_quotes && !double_quotes && current.trim().is_empty() {
                current.clear();
            }

            match current_char {
                _ if current_char.is_whitespace() && !single_quotes && !double_quotes => {
                    if !current.trim().is_empty() {
                        tokens.push(Token::Word(current.clone(), Quoting::Unquoted));
                        current.clear();
                    }
                    // only push if last token is not whitespace
                    match tokens.last() {
                        Some(Token::Whitespace) => {}
                        _ => {
                            tokens.push(Token::Whitespace);
                            current.clear();
                        }
                    }
                }
                '\'' if !double_quotes => {
                    if !current.is_empty() {
                        let quoting = if single_quotes {
                            Quoting::SingleQuoted
                        } else {
                            Quoting::Unquoted
                        };
                        tokens.push(Token::Word(current.clone(), quoting));
                        current.clear();
                    }
                    single_quotes = !single_quotes;
                }
                '"' if !single_quotes => {
                    if !current.is_empty() {
                        let quoting = if double_quotes {
                            Quoting::DoubleQuoted
                        } else {
                            Quoting::Unquoted
                        };
                        tokens.push(Token::Word(current.clone(), quoting));
                        current.clear();
                    }
                    double_quotes = !double_quotes;
                }
                '&' if !single_quotes && !double_quotes => {
                    if !current.trim().is_empty() {
                        tokens.push(Token::Word(current.clone(), Quoting::Unquoted));
                        current.clear();
                    }
                    if let Some(&ch) = chars.peek() {
                        if ch == '&' {
                            chars.next();
                            tokens.push(Token::Operator(Operator::And));
                        } else {
                            tokens.push(Token::Operator(Operator::Andpercent));
                        }
                    } else {
                        tokens.push(Token::Operator(Operator::Andpercent));
                    }
                    current.clear();
                }
                '|' if !single_quotes && !double_quotes => {
                    if !current.trim().is_empty() {
                        tokens.push(Token::Word(current.clone(), Quoting::Unquoted));
                        current.clear();
                    }
                    if let Some(&ch) = chars.peek() {
                        if ch == '|' {
                            chars.next();
                            tokens.push(Token::Operator(Operator::Or));
                        } else {
                            tokens.push(Token::Operator(Operator::Pipe));
                        }
                    } else {
                        tokens.push(Token::Operator(Operator::Pipe));
                    }
                    current.clear();
                }
                ';' if !single_quotes && !double_quotes => {
                    if !current.trim().is_empty() {
                        tokens.push(Token::Word(current.clone(), Quoting::Unquoted));
                        current.clear();
                    }
                    tokens.push(Token::Operator(Operator::Semicolon));
                    current.clear();
                }
                '\\' => {
                    if let Some(&ch) = chars.peek() {
                        chars.next();
                        match ch {
                            'n' => current.push('\n'),
                            't' => current.push('\t'),
                            'r' => current.push('\r'),
                            '0' => current.push('\0'),
                            ch => current.push(ch),
                        };
                    }
                }
                _ => current.push(current_char),
            }
        }

        // for now if a quote is opened and not closed the whole content is just discarded
        if !current.trim().is_empty() && !single_quotes && !double_quotes {
            tokens.push(Token::Word(current, Quoting::Unquoted));
        }

        tokens
    }

    fn parse(&self, tokens: Vec<Token>) -> Option<Command> {
        if tokens.is_empty() {
            None
        } else {
            let args: Vec<String> = tokens
                .into_iter()
                .filter_map(|token| match token {
                    Token::Word(word, Quoting::SingleQuoted) => Some(word),
                    Token::Word(word, _) => Some(
                        self.variable_regex
                            .replace_all(word.as_str(), |caps: &regex::Captures| {
                                let k = &caps[1];
                                env::var(k).unwrap_or_default()
                            })
                            .into_owned(),
                    ),
                    _ => None,
                })
                .collect();

            match args[0].as_str() {
                "exit" => Some(Command::Builtin(BuiltinCommand::Exit)),
                "cd" => Some(Command::Builtin(BuiltinCommand::Cd(args))),
                command => {
                    let external_command = ExternalCommand::new(command.to_string(), args);
                    Some(Command::External(external_command))
                }
            }
        }
    }
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
        let parser = Parser::new();
        loop {
            print!("\n$ ");
            self.stdout_handle.flush().unwrap();

            let mut input = String::new();
            let bytes_read = self.stdin_handle.lock().read_line(&mut input).unwrap();

            if bytes_read == 0 {
                println!("\nexit");
                exit(0);
            }

            let tokens = parser.tokenize(input.as_str());

            if let Some(command) = parser.parse(tokens) {
                self.execute(command)?;
            }
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


struct StringParser {
    string: String
}
struct OrParser<P1,P2> {
    parser1: P1,
    parser2: P2
}
struct NeverParser {}
struct MapParser<P1,F,OldOutput>
{
    parser: P1,
    function: F,
    old_output: PhantomData<OldOutput>
}
struct ManyParser<P> {
    parser: P
}
struct DiscardAndThenKeepParser<P1,P2,DiscardedOutput> {
    parser1: P1,
    parser2: P2,
    discarded_output: PhantomData<DiscardedOutput>
    
}
struct KeepAndThenDiscardParser<P1,P2,DiscardedOutput> {
    parser1: P1,
    parser2: P2,
    discarded_output: PhantomData<DiscardedOutput>
    
}

trait ParserT<Output> {
    fn parse(&self, input: String) -> Option<(Output, String)>;
    fn or<P>(&self, parser: P) -> OrParser<Box<&Self>, P>
    where
        P: ParserT<Output>
    {
        OrParser {
            parser1: Box::new(self),
            parser2: parser
        }   
    }
    fn map<F, NewOutput>(&self, f: F) -> MapParser<&Self, F, Output>
    where
        F: Fn(Output) -> NewOutput
    {
        MapParser { parser: self, function: f, old_output: PhantomData }
    }
    fn keep_and_then_discard<DiscardedOutput, P>(&self, parser: P) -> KeepAndThenDiscardParser<&Self, P, DiscardedOutput>
    where
        P: ParserT<DiscardedOutput>
    {
        KeepAndThenDiscardParser { parser1: self, parser2: parser, discarded_output: PhantomData }
    }
    fn discard_and_then_keep<NewOutput, P>(&self, parser: P) -> DiscardAndThenKeepParser<&Self, P, Output>
    where
        P: ParserT<NewOutput>
    {
        DiscardAndThenKeepParser { parser1: self, parser2: parser, discarded_output: PhantomData }
    }
}

impl<Output,P> ParserT<Output> for &P
where
    P: ParserT<Output>
{
    fn parse(&self, input: String) -> Option<(Output, String)> {
        (**self).parse(input)
    }
}
impl<Output,P> ParserT<Output> for Box<P>
where
    P: ParserT<Output>
{
    fn parse(&self, input: String) -> Option<(Output, String)> {
        (**self).parse(input)
    }
}

impl<DiscardedOutput,Output,P1,P2> ParserT<Output> for KeepAndThenDiscardParser<P1,P2,DiscardedOutput>
where
    P1: ParserT<Output>,
    P2: ParserT<DiscardedOutput>
{
    fn parse(&self, input: String) -> Option<(Output, String)> {
        let (result, _remaining) = self.parser1.parse(input)?;
        let (_discarded_result, remaining) = self.parser2.parse(_remaining)?;
        Some((result, remaining))
    }
}

impl<DiscardedOutput,Output,P1,P2> ParserT<Output> for DiscardAndThenKeepParser<P1,P2,DiscardedOutput>
where
    P1: ParserT<DiscardedOutput>,
    P2: ParserT<Output>
{
    fn parse(&self, input: String) -> Option<(Output, String)> {
        let (_result, remaining) = self.parser1.parse(input)?;
        self.parser2.parse(remaining)
    }
}

impl<Output, P> ParserT<Vec<Output>> for ManyParser<P>
where
    P: ParserT<Output>
{
    fn parse(&self, input: String) -> Option<(Vec<Output>, String)> {
        let mut remaining_input: String = input.clone();
        let mut result: Vec<Output> = Vec::new();
        while let Some((current_result,current_remaining_input)) = self.parser.parse(input.clone()) {
            result.push(current_result);
            remaining_input = current_remaining_input;
        }
        Some((result,remaining_input))
    }
}

impl<Output, P1, NewOutput, F> ParserT<NewOutput> for MapParser<P1, F, Output>
where
    P1: ParserT<Output>,
    F: Fn(Output) -> NewOutput
{
    fn parse(&self, input: String) -> Option<(NewOutput, String)> {
        self.parser.parse(input).map(
            |t| ((self.function)(t.0), t.1)
        )
    }
}

impl ParserT<String> for StringParser {
    fn parse(&self, input: String) -> Option<(String, String)> {
        if let Some(suffix) = input.strip_prefix(self.string.as_str()) {
            Some((input.clone(),suffix.to_owned()))
        } else {
            None
        }
    }
}

impl<Output, P1, P2> ParserT<Output> for OrParser<P1, P2>
where
    P1: ParserT<Output>,
    P2: ParserT<Output>
{
    fn parse(&self, input: String) -> Option<(Output, String)> {
        self.parser1.parse(input.clone()).or(self.parser2.parse(input))
    }
}

impl<Output> ParserT<Output> for NeverParser {
    fn parse(&self, input: String) -> Option<(Output, String)> {
        None
    }
}

//struct ParserS<Output> {
//    parser: Rc<dyn ParserT<Output>>
//}
//impl<Output> ParserT<Output> for Rc<dyn ParserT<Output>> {
//    fn parse(&self, input: String) -> Option<(Output, String)> {
//        (**self).parse(input)
//    }
//}



fn main() {
//    let mut shell = Shell::new().expect("Failed to spawn shell");
//    shell.run().expect("Failed to run shell");
    let test: String = " cd     /home".to_owned();
    let whitespace_parser: OrParser<Box<&StringParser>, StringParser>  =
        StringParser { string: " ".to_owned() }
        .or(StringParser { string: "    ".to_owned() });
    let cd_parser =
        whitespace_parser
        .discard_and_then_keep(StringParser { string: "cd".to_owned() } )

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
