use std::ffi::OsString;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Bind,
    List,
    Put { local: OsString, remote: OsString },
    Get { remote: OsString, local: OsString },
    Run { argv: Vec<OsString> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

pub fn parse<I>(args: I) -> Result<Command, ParseError>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    if args.is_empty() {
        return Err(ParseError(usage()));
    }

    if args[0] == "--" {
        if args.len() == 1 {
            return Err(ParseError("expected a remote command after `--`".into()));
        }
        return Ok(Command::Run {
            argv: args[1..].to_vec(),
        });
    }

    match args[0].to_string_lossy().as_ref() {
        "bind" => {
            if args.len() == 1 {
                Ok(Command::Bind)
            } else {
                Err(ParseError("usage: uu bind".into()))
            }
        }
        "list" => {
            if args.len() == 1 {
                Ok(Command::List)
            } else {
                Err(ParseError("usage: uu list".into()))
            }
        }
        "put" => {
            if args.len() == 3 {
                Ok(Command::Put {
                    local: args[1].clone(),
                    remote: args[2].clone(),
                })
            } else {
                Err(ParseError("usage: uu put <local> <remote>".into()))
            }
        }
        "get" => {
            if args.len() == 3 {
                Ok(Command::Get {
                    remote: args[1].clone(),
                    local: args[2].clone(),
                })
            } else {
                Err(ParseError("usage: uu get <remote> <local>".into()))
            }
        }
        _ => Ok(Command::Run { argv: args }),
    }
}

pub fn usage() -> String {
    [
        "usage:",
        "  uu <command...>",
        "  uu put <local> <remote>",
        "  uu get <remote> <local>",
        "  uu bind",
        "  uu list",
        "  uu -- <remote command...>   # escape reserved words",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<OsString> {
        xs.iter().map(OsString::from).collect()
    }

    #[test]
    fn arbitrary_command_is_passthrough() {
        assert_eq!(
            parse(v(&["git", "status", "--short"])).unwrap(),
            Command::Run {
                argv: v(&["git", "status", "--short"])
            }
        );
    }

    #[test]
    fn double_dash_escapes_reserved_words() {
        assert_eq!(
            parse(v(&["--", "put", "a", "b"])).unwrap(),
            Command::Run {
                argv: v(&["put", "a", "b"])
            }
        );
    }

    #[test]
    fn put_and_get_are_strict() {
        assert!(matches!(parse(v(&["put", "a", "b"])), Ok(Command::Put { .. })));
        assert!(parse(v(&["put", "a"])).is_err());
        assert!(matches!(parse(v(&["get", "a", "b"])), Ok(Command::Get { .. })));
        assert!(parse(v(&["get", "a", "b", "c"])).is_err());
    }
}
