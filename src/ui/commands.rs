//! Slash-command parsing (PRD §12). Pure, so it is unit-tested.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// A chat message (text not starting with `/`, or `//...`).
    Text(String),
    Help,
    Invite {
        stop: bool,
        /// `/invite CODE`: only the device with this invite code (unparsed).
        code: Option<String>,
    },
    Add(Option<String>),
    Accept(Option<String>),
    Reject(Option<String>),
    Verify(Option<String>),
    Connect(Option<String>),
    Disconnect(Option<String>),
    Chat(Option<String>),
    Remove(Option<String>),
    Alias {
        name: String,
        alias: Option<String>,
    },
    Send(Option<String>),
    Call(Option<String>),
    Answer,
    Hangup,
    Mute(Option<bool>),
    Devices,
    VoiceTest {
        stop: bool,
    },
    Mic(Option<String>),
    Speaker(Option<String>),
    Codename(Option<String>),
    MyKey {
        save: bool,
    },
    Theme(Option<String>),
    Effects(Option<bool>),
    Flash(Option<bool>),
    Contacts,
    Clear,
    Panic,
    PanicWipe(Option<bool>),
    Quit,
    Yes,
    No,
    Unknown(String),
}

pub const HELP: &[(&str, &str)] = &[
    (
        "/invite CODE",
        "invite the device with this invite code (they tell you their code); shows your Contact Key",
    ),
    (
        "/invite",
        "open a pairing window for anyone with your Contact Key (check the request code)",
    ),
    ("/invite stop", "close the pairing window"),
    (
        "/add KEY",
        "add a contact: paste their Contact Key (or /add alone to open a .ghostkey file)",
    ),
    (
        "/accept, /reject",
        "answer a pair request or an incoming file",
    ),
    (
        "/verify NAME",
        "verify a contact with safety words (on a voice/video call)",
    ),
    (
        "/chat NAME",
        "switch to a conversation (or click the name on the left)",
    ),
    (
        "/connect [NAME]",
        "connect now (GhostChat also retries automatically)",
    ),
    ("/disconnect [NAME]", "close the secure channel"),
    (
        "/send [FILE]",
        "send a file (or drag a file onto the window)",
    ),
    (
        "/call [NAME]",
        "start a voice call (E2EE) with a connected contact",
    ),
    ("/answer", "answer an incoming call"),
    (
        "/hangup",
        "end the call, cancel it, or decline an incoming call",
    ),
    (
        "/mute [on|off]",
        "mute or unmute your microphone during calls",
    ),
    ("/devices", "list microphones and speakers"),
    (
        "/voicetest",
        "record 5 seconds and play them back: check microphone and speakers before a call",
    ),
    (
        "/mic N|default",
        "choose the microphone for calls (number from /devices)",
    ),
    (
        "/speaker N|default",
        "choose the speakers for calls (number from /devices)",
    ),
    ("/codename NAME", "change your codename"),
    (
        "/alias NAME NEW",
        "show a contact under your own name for them (/alias NAME to reset)",
    ),
    ("/remove NAME", "delete a contact"),
    ("/contacts", "list contacts and their trust state"),
    (
        "/mykey [save]",
        "show your Contact Key and randomart (save = write a .ghostkey file)",
    ),
    ("/theme green|amber|ice", "change colours"),
    ("/fx on|off", "retro effects (scanlines, flicker)"),
    (
        "/flash on|off",
        "flash the taskbar / Dock icon when a message arrives in the background",
    ),
    (
        "/clear",
        "wipe this conversation from the screen and memory",
    ),
    (
        "/panic  or  F12",
        "wipe everything in memory and exit immediately",
    ),
    (
        "/panicwipe on|off",
        "also delete identity + contacts on panic (default off)",
    ),
    ("/quit", "close GhostChat"),
];

fn arg(rest: &str) -> Option<String> {
    let r = rest.trim();
    (!r.is_empty()).then(|| r.to_string())
}

fn on_off_arg(rest: &str) -> Option<bool> {
    match rest.trim().to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

pub fn parse(input: &str) -> Cmd {
    let trimmed = input.trim_end_matches(['\r', '\n']);
    if let Some(literal) = trimmed.strip_prefix("//") {
        return Cmd::Text(format!("/{literal}"));
    }
    let Some(body) = trimmed.trim_start().strip_prefix('/') else {
        return Cmd::Text(trimmed.to_string());
    };
    let (name, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    match name.to_ascii_lowercase().as_str() {
        "help" | "h" | "?" => Cmd::Help,
        "invite" => {
            let stop = matches!(
                rest.trim().to_ascii_lowercase().as_str(),
                "stop" | "close" | "off"
            );
            Cmd::Invite {
                stop,
                code: if stop { None } else { arg(rest) },
            }
        }
        "add" => Cmd::Add(arg(rest)),
        "accept" => Cmd::Accept(arg(rest)),
        "reject" | "decline" => Cmd::Reject(arg(rest)),
        "verify" => Cmd::Verify(arg(rest)),
        "connect" => Cmd::Connect(arg(rest)),
        "disconnect" => Cmd::Disconnect(arg(rest)),
        "chat" | "open" => Cmd::Chat(arg(rest)),
        "remove" | "delete" => Cmd::Remove(arg(rest)),
        "alias" | "rename" => {
            let rest = rest.trim();
            match rest.split_once(char::is_whitespace) {
                Some((name, alias)) => Cmd::Alias {
                    name: name.to_string(),
                    alias: arg(alias),
                },
                None if !rest.is_empty() => Cmd::Alias {
                    name: rest.to_string(),
                    alias: None,
                },
                None => Cmd::Unknown("usage: /alias NAME NEWNAME".into()),
            }
        }
        "send" | "file" => Cmd::Send(arg(rest)),
        "call" => Cmd::Call(arg(rest)),
        "answer" | "pickup" => Cmd::Answer,
        "hangup" | "endcall" => Cmd::Hangup,
        "mute" => Cmd::Mute(on_off_arg(rest)),
        "devices" | "audio" => Cmd::Devices,
        "voicetest" | "mictest" => Cmd::VoiceTest {
            stop: matches!(rest.trim().to_ascii_lowercase().as_str(), "stop" | "off"),
        },
        "mic" | "microphone" => Cmd::Mic(arg(rest)),
        "speaker" | "speakers" => Cmd::Speaker(arg(rest)),
        "codename" | "nick" => Cmd::Codename(arg(rest)),
        "mykey" | "key" => Cmd::MyKey {
            save: rest.trim().eq_ignore_ascii_case("save"),
        },
        "theme" => Cmd::Theme(arg(rest)),
        "fx" | "effects" => Cmd::Effects(on_off_arg(rest)),
        "flash" => Cmd::Flash(on_off_arg(rest)),
        "contacts" | "list" => Cmd::Contacts,
        "clear" | "cls" => Cmd::Clear,
        "panic" => Cmd::Panic,
        "panicwipe" => Cmd::PanicWipe(on_off_arg(rest)),
        "quit" | "exit" => Cmd::Quit,
        "yes" | "y" => Cmd::Yes,
        "no" | "n" => Cmd::No,
        other => Cmd::Unknown(format!("unknown command /{other} — type /help")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_a_message() {
        assert_eq!(parse("hello"), Cmd::Text("hello".into()));
        assert_eq!(parse("  hi there"), Cmd::Text("  hi there".into()));
        assert_eq!(parse("//not a command"), Cmd::Text("/not a command".into()));
    }

    #[test]
    fn commands_with_and_without_args() {
        assert_eq!(parse("/help"), Cmd::Help);
        assert_eq!(parse("/HELP"), Cmd::Help);
        assert_eq!(
            parse("/invite"),
            Cmd::Invite {
                stop: false,
                code: None
            }
        );
        assert_eq!(
            parse("/invite stop"),
            Cmd::Invite {
                stop: true,
                code: None
            }
        );
        assert_eq!(
            parse("/invite 7f3a-91c2-xk4p"),
            Cmd::Invite {
                stop: false,
                code: Some("7f3a-91c2-xk4p".into())
            }
        );
        assert_eq!(parse("/add"), Cmd::Add(None));
        assert_eq!(
            parse("/add  GHOSTKEY1.A.B.C "),
            Cmd::Add(Some("GHOSTKEY1.A.B.C".into()))
        );
        assert_eq!(
            parse("/verify nightowl"),
            Cmd::Verify(Some("nightowl".into()))
        );
        assert_eq!(parse("/accept"), Cmd::Accept(None));
        assert_eq!(parse("/mykey save"), Cmd::MyKey { save: true });
        assert_eq!(parse("/fx off"), Cmd::Effects(Some(false)));
        assert_eq!(parse("/panic"), Cmd::Panic);
        assert_eq!(
            parse("/send C:\\a b.txt"),
            Cmd::Send(Some("C:\\a b.txt".into()))
        );
    }

    #[test]
    fn alias_forms() {
        assert_eq!(
            parse("/alias NIGHTOWL Contact-1"),
            Cmd::Alias {
                name: "NIGHTOWL".into(),
                alias: Some("Contact-1".into())
            }
        );
        assert_eq!(
            parse("/alias NIGHTOWL"),
            Cmd::Alias {
                name: "NIGHTOWL".into(),
                alias: None
            }
        );
        assert!(matches!(parse("/alias"), Cmd::Unknown(_)));
    }

    #[test]
    fn on_off_toggles() {
        assert_eq!(parse("/flash OFF"), Cmd::Flash(Some(false)));
        assert_eq!(parse("/flash on"), Cmd::Flash(Some(true)));
        assert_eq!(parse("/flash maybe"), Cmd::Flash(None));
        assert_eq!(parse("/fx off"), Cmd::Effects(Some(false)));
        assert_eq!(parse("/panicwipe on"), Cmd::PanicWipe(Some(true)));
    }

    #[test]
    fn call_commands() {
        assert_eq!(parse("/call NIGHTOWL"), Cmd::Call(Some("NIGHTOWL".into())));
        assert_eq!(parse("/call"), Cmd::Call(None));
        assert_eq!(parse("/answer"), Cmd::Answer);
        assert_eq!(parse("/hangup"), Cmd::Hangup);
        assert_eq!(parse("/mute"), Cmd::Mute(None));
        assert_eq!(parse("/mute off"), Cmd::Mute(Some(false)));
        assert_eq!(parse("/devices"), Cmd::Devices);
        assert_eq!(parse("/voicetest"), Cmd::VoiceTest { stop: false });
        assert_eq!(parse("/mictest stop"), Cmd::VoiceTest { stop: true });
        assert_eq!(parse("/mic 2"), Cmd::Mic(Some("2".into())));
        assert_eq!(
            parse("/speaker default"),
            Cmd::Speaker(Some("default".into()))
        );
    }

    #[test]
    fn unknown_command() {
        assert!(matches!(parse("/frobnicate"), Cmd::Unknown(_)));
    }
}
