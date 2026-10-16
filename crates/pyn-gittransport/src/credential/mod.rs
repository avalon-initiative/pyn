//! Git's credential flow in process: credentials in the URL, `credential.helper` entries, askpass, then a prompt.

mod helper;
mod prompt;
mod store;

use pyn_git::Config;
use reqwest::Url;

use crate::env::Env;
use crate::error::{Error, Result};
use crate::url::{scoped_value, scoped_values};
pub use helper::{Helper, Op};
pub use prompt::{Prompt, SystemPrompt, askpass_program};
pub use store::StoreFiles;

const MAX_PROMPTS: usize = 3;

/// One credential as the helper protocol carries it. The password is never printed.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credential {
    pub protocol: String,
    /// The host, with `:port` when the URL names one.
    pub host: String,
    pub path: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Attributes a helper returned that are handed back on `store` and `erase`.
    pub extra: Vec<(String, String)>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

fn check(field: &str, value: &str) -> Result<()> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(Error::InvalidCredential(field.to_owned()));
    }
    Ok(())
}

impl Credential {
    pub fn for_url(url: &Url, with_path: bool) -> Self {
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => String::new(),
        };
        let path = url.path().trim_start_matches('/');
        Self {
            protocol: url.scheme().to_owned(),
            host,
            path: (with_path && !path.is_empty()).then(|| path.to_owned()),
            ..Self::default()
        }
    }

    /// Rejects control characters in any field, since each is a line of the helper protocol.
    pub fn validate(&self) -> Result<()> {
        check("protocol", &self.protocol)?;
        check("host", &self.host)?;
        for (k, v) in [
            ("path", &self.path),
            ("username", &self.username),
            ("password", &self.password),
        ] {
            if let Some(v) = v {
                check(k, v)?;
            }
        }
        for (k, v) in &self.extra {
            check(k, k)?;
            check(k, v)?;
        }
        Ok(())
    }

    pub fn to_lines(&self) -> Result<String> {
        self.validate()?;
        let mut out = format!("protocol={}\nhost={}\n", self.protocol, self.host);
        for (k, v) in [
            ("path", &self.path),
            ("username", &self.username),
            ("password", &self.password),
        ] {
            if let Some(v) = v {
                out.push_str(&format!("{k}={v}\n"));
            }
        }
        for (k, v) in &self.extra {
            out.push_str(&format!("{k}={v}\n"));
        }
        out.push('\n');
        Ok(out)
    }

    /// Merges a helper's `key=value` lines in; returns whether it asked to stop (`quit`).
    /// Nothing is merged when any value holds a control character.
    pub fn read_lines(&mut self, text: &str) -> Result<bool> {
        let mut next = self.clone();
        let mut quit = false;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            check(k, k)?;
            check(k, v)?;
            match k {
                "username" => next.username = Some(v.to_owned()),
                "password" => next.password = Some(v.to_owned()),
                "quit" => quit = matches!(v, "1" | "true"),
                "protocol" | "host" | "path" | "url" => {}
                _ => {
                    next.extra.retain(|(ek, _)| ek != k);
                    next.extra.push((k.to_owned(), v.to_owned()));
                }
            }
        }
        *self = next;
        Ok(quit)
    }

    fn complete(&self) -> bool {
        self.username.is_some() && self.password.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Url,
    Helper,
    Prompt,
}

#[derive(Debug, Clone)]
pub struct Offer {
    pub cred: Credential,
    pub source: Source,
}

fn origin(url: &Url) -> (String, String, Option<u16>) {
    (
        url.scheme().to_owned(),
        url.host_str().unwrap_or("").to_ascii_lowercase(),
        url.port_or_known_default(),
    )
}

/// Produces credentials for one remote, one candidate per call, and reports back what worked.
pub struct Flow {
    config: Config,
    env: Env,
    prompt: Box<dyn Prompt>,
    home: (String, String, Option<u16>),
    embedded: Option<(String, Option<String>)>,
    tried: Vec<(String, String)>,
    prompts: usize,
}

impl Flow {
    /// `initial` is the URL as given: credentials in it belong to its host and no other.
    pub fn new(config: Config, env: Env, prompt: Box<dyn Prompt>, initial: &Url) -> Self {
        let user = crate::url::decode(initial.username());
        let embedded =
            (!user.is_empty()).then(|| (user, initial.password().map(crate::url::decode)));
        Self {
            config,
            env,
            prompt,
            home: origin(initial),
            embedded,
            tried: Vec::new(),
            prompts: 0,
        }
    }

    fn embedded_for(&self, url: &Url) -> Option<&(String, Option<String>)> {
        self.embedded.as_ref().filter(|_| origin(url) == self.home)
    }

    fn helpers(&self, url: &Url) -> Vec<Helper> {
        scoped_values(&self.config, "credential", "helper", url)
            .into_iter()
            .filter_map(|v| helper::resolve(v, &self.env))
            .collect()
    }

    fn query(&self, url: &Url) -> Credential {
        let with_path = scoped_value(&self.config, "credential", "usehttppath", url)
            .is_some_and(|v| !crate::url::is_false(v));
        let mut q = Credential::for_url(url, with_path);
        q.username = self.embedded_for(url).map(|(u, _)| u.clone()).or_else(|| {
            scoped_value(&self.config, "credential", "username", url).map(str::to_owned)
        });
        q
    }

    fn fresh(&self, user: &str, pw: &str) -> bool {
        !self.tried.iter().any(|(u, p)| u == user && p == pw)
    }

    fn offer(&mut self, cred: Credential, source: Source) -> Option<Offer> {
        self.tried.push((
            cred.username.clone().unwrap_or_default(),
            cred.password.clone().unwrap_or_default(),
        ));
        Some(Offer { cred, source })
    }

    /// The next credential to try for `url`, or `None` when every source is spent.
    pub fn next(&mut self, url: &Url) -> Result<Option<Offer>> {
        let query = self.query(url);
        query.validate()?;
        if let Some((user, pw)) = self.embedded_for(url) {
            check("username", user)?;
            if let Some(pw) = pw {
                check("password", pw)?;
            }
        }
        if let Some((user, Some(pw))) = self.embedded_for(url).cloned()
            && self.fresh(&user, &pw)
        {
            let mut c = query.clone();
            c.username = Some(user);
            c.password = Some(pw);
            return Ok(self.offer(c, Source::Url));
        }
        let mut got = query.clone();
        for h in self.helpers(url) {
            let quit = helper::run(&h, Op::Get, &mut got, &self.env)?;
            if got.complete() || quit {
                break;
            }
        }
        if got.complete()
            && self.fresh(
                got.username.as_deref().unwrap_or(""),
                got.password.as_deref().unwrap_or(""),
            )
        {
            return Ok(self.offer(got, Source::Helper));
        }
        if let Some((user, None)) = self.embedded_for(url).cloned()
            && self.fresh(&user, "")
        {
            let mut c = query.clone();
            c.username = Some(user);
            c.password = Some(String::new());
            return Ok(self.offer(c, Source::Url));
        }
        if self.prompts >= MAX_PROMPTS {
            return Ok(None);
        }
        self.prompts += 1;
        let shown = crate::url::RemoteUrl::Http(url.clone()).display();
        let mut c = query;
        let user = match c.username.clone() {
            Some(u) => u,
            None => self
                .prompt
                .ask(&format!("Username for '{shown}': "), false)?,
        };
        let at = Url::parse(&shown)
            .ok()
            .map(|mut u| {
                let _ = u.set_username(&user);
                u.to_string()
            })
            .unwrap_or(shown);
        let pw = self.prompt.ask(&format!("Password for '{at}': "), true)?;
        check("username", &user)?;
        check("password", &pw)?;
        c.username = Some(user);
        c.password = Some(pw);
        Ok(self.offer(c, Source::Prompt))
    }

    /// The credential worked: helpers are asked to remember it (one that came from the URL is not copied anywhere).
    pub fn approve(&mut self, offer: &Offer, url: &Url) {
        if offer.source == Source::Url {
            return;
        }
        for h in self.helpers(url) {
            let _ = helper::run(&h, Op::Store, &mut offer.cred.clone(), &self.env);
        }
    }

    /// The server refused the credential: helpers are asked to forget it.
    pub fn reject(&mut self, offer: &Offer, url: &Url) {
        if offer.source == Source::Url {
            return;
        }
        for h in self.helpers(url) {
            let _ = helper::run(&h, Op::Erase, &mut offer.cred.clone(), &self.env);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<String>, std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Prompt for Scripted {
        fn ask(&mut self, prompt: &str, _secret: bool) -> Result<String> {
            self.1.lock().unwrap().push(prompt.to_owned());
            if self.0.is_empty() {
                return Err(crate::Error::Credential("no more answers".into()));
            }
            Ok(self.0.remove(0))
        }
    }

    fn flow(
        config: &str,
        url: &str,
        answers: &[&str],
    ) -> (Flow, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let prompt = Scripted(
            answers.iter().map(|s| (*s).to_owned()).collect(),
            asked.clone(),
        );
        let env = Env::default().with("PATH", "/usr/bin:/bin");
        let f = Flow::new(
            Config::parse(config.as_bytes()).unwrap(),
            env,
            Box::new(prompt),
            &Url::parse(url).unwrap(),
        );
        (f, asked)
    }

    fn pair(o: &Offer) -> (&str, &str) {
        (
            o.cred.username.as_deref().unwrap(),
            o.cred.password.as_deref().unwrap(),
        )
    }

    #[test]
    fn url_credentials_come_first_and_only_for_their_host() {
        let (mut f, _) = flow("", "https://alice:pw@example.com/r.git", &[]);
        let url = Url::parse("https://example.com/r.git").unwrap();
        let o = f.next(&url).unwrap().unwrap();
        assert_eq!((pair(&o), o.source), (("alice", "pw"), Source::Url));
        let other = Url::parse("https://evil.example.org/r.git").unwrap();
        let (mut f2, asked) = flow("", "https://alice:pw@example.com/r.git", &["x", "y"]);
        let o = f2.next(&other).unwrap().unwrap();
        assert_eq!((pair(&o), o.source), (("x", "y"), Source::Prompt));
        assert_eq!(asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_user_only_url_is_a_token_after_helpers_have_nothing() {
        let (mut f, asked) = flow("", "https://tok@example.com/r.git", &["pw"]);
        let url = Url::parse("https://example.com/r.git").unwrap();
        let o = f.next(&url).unwrap().unwrap();
        assert_eq!(pair(&o), ("tok", ""));
        let o = f.next(&url).unwrap().unwrap();
        assert_eq!((pair(&o), o.source), (("tok", "pw"), Source::Prompt));
        assert_eq!(asked.lock().unwrap().len(), 1, "only the password is asked");
    }

    #[test]
    fn helpers_are_asked_before_prompting_and_scoped_sections_apply() {
        let config = "[credential]\n\thelper = \"!f() { echo username=generic; echo password=g; }; f\"\n\
                      [credential \"https://example.com\"]\n\thelper =\n\thelper = \"!f() { echo username=scoped; echo password=s; }; f\"\n";
        let (mut f, asked) = flow(config, "https://example.com/r.git", &[]);
        let url = Url::parse("https://example.com/r.git").unwrap();
        let o = f.next(&url).unwrap().unwrap();
        assert_eq!((pair(&o), o.source), (("scoped", "s"), Source::Helper));
        assert!(asked.lock().unwrap().is_empty());
        let o = f.next(&url);
        assert!(
            o.is_err(),
            "the helper repeats itself, so the flow falls through to a prompt that fails"
        );
    }

    #[test]
    fn credential_username_and_use_http_path_shape_the_query() {
        let config = "[credential]\n\tusername = carol\n\tuseHttpPath = true\n\
                      \thelper = \"!f() { cat >/dev/null; echo username=carol; echo password=cp; }; f\"\n";
        let (mut f, _) = flow(config, "https://example.com/team/r.git", &[]);
        let url = Url::parse("https://example.com/team/r.git").unwrap();
        let q = f.query(&url);
        assert_eq!(q.username.as_deref(), Some("carol"));
        assert_eq!(q.path.as_deref(), Some("team/r.git"));
        assert_eq!(pair(&f.next(&url).unwrap().unwrap()), ("carol", "cp"));
    }

    #[test]
    fn prompts_are_capped_and_never_show_passwords() {
        let (mut f, asked) = flow(
            "",
            "https://example.com/r.git",
            &["u", "p1", "u", "p2", "u", "p3"],
        );
        let url = Url::parse("https://example.com/r.git").unwrap();
        for _ in 0..3 {
            assert!(f.next(&url).unwrap().is_some());
        }
        assert!(f.next(&url).unwrap().is_none());
        assert!(asked.lock().unwrap().iter().all(|p| !p.contains("p1")));
        assert_eq!(
            asked.lock().unwrap()[0],
            "Username for 'https://example.com/r.git': "
        );
        assert_eq!(
            asked.lock().unwrap()[1],
            "Password for 'https://u@example.com/r.git': "
        );
    }

    #[test]
    fn control_characters_are_rejected_in_every_field() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            for field in 0..5 {
                let mut c = Credential {
                    protocol: "https".into(),
                    host: "h".into(),
                    ..Credential::default()
                };
                match field {
                    0 => c.protocol = bad.into(),
                    1 => c.host = bad.into(),
                    2 => c.path = Some(bad.into()),
                    3 => c.username = Some(bad.into()),
                    _ => c.password = Some(bad.into()),
                }
                let e = c.to_lines().unwrap_err();
                assert!(matches!(e, Error::InvalidCredential(_)), "{e}");
            }
        }
        let ok = Credential {
            protocol: "https".into(),
            host: "h:1".into(),
            path: Some("a b/c.git".into()),
            username: Some("us\u{e9}r".into()),
            password: Some("p=w d\t".into()),
            ..Credential::default()
        };
        assert!(ok.to_lines().is_ok());
    }

    #[test]
    fn helper_output_with_control_characters_is_rejected_whole() {
        let mut c = Credential::default();
        assert!(c.read_lines("username=ok\npassword=a\rb\n").is_err());
        assert!(c.username.is_none(), "nothing is merged");
        assert!(c.read_lines("username=ok\npassword=a\0b\n").is_err());
        assert!(c.read_lines("username=ok\npassword=fine\n").is_ok());
        assert_eq!(c.password.as_deref(), Some("fine"));
    }

    #[test]
    fn url_userinfo_with_encoded_control_characters_is_rejected() {
        for userinfo in ["al%0Aice:pw", "alice:p%0Dw", "alice:p%00w"] {
            let (mut f, _) = flow("", &format!("https://{userinfo}@example.com/r.git"), &[]);
            let url = Url::parse("https://example.com/r.git").unwrap();
            let e = f.next(&url).unwrap_err();
            assert!(matches!(e, Error::InvalidCredential(_)), "{userinfo}: {e}");
        }
        let (mut f, _) = flow("", "https://al%20ice:p%40w@example.com/r.git", &[]);
        let url = Url::parse("https://example.com/r.git").unwrap();
        assert_eq!(pair(&f.next(&url).unwrap().unwrap()), ("al ice", "p@w"));
    }

    #[test]
    fn prompt_answers_with_control_characters_are_rejected() {
        let (mut f, _) = flow("", "https://example.com/r.git", &["u", "p\nq"]);
        let url = Url::parse("https://example.com/r.git").unwrap();
        assert!(matches!(
            f.next(&url).unwrap_err(),
            Error::InvalidCredential(_)
        ));
    }

    #[test]
    fn configured_username_with_control_characters_is_rejected() {
        let (mut f, _) = flow(
            "[credential]\n\tusername = \"a\\nb\"\n",
            "https://example.com/r.git",
            &[],
        );
        let url = Url::parse("https://example.com/r.git").unwrap();
        assert!(matches!(
            f.next(&url).unwrap_err(),
            Error::InvalidCredential(_)
        ));
    }

    #[test]
    fn debug_output_hides_the_password() {
        let c = Credential {
            password: Some("hunter2".into()),
            ..Credential::default()
        };
        assert!(!format!("{c:?}").contains("hunter2"));
        let o = Offer {
            cred: c,
            source: Source::Prompt,
        };
        assert!(!format!("{o:?}").contains("hunter2"));
    }
}
