//! A small query language for runs, written in the search field of the runs table.
//!
//! ```text
//! resnet                      words match the name, experiment, tags, id and notes
//! status:failed               `:` contains (text) or equals (numbers)
//! lr<0.01 batch_size>=64      parameters by full dotted key or by their last segment
//! optimizer.name=adam         `=` equals, `!=` differs (text ignores case)
//! loss[val]<0.3               a metric's last value; `[val]` names the context
//! loss[val].min<0.25          `.min`, `.max`, `.last` or `.count` picks the statistic
//! duration>10m age<2d         durations in s, m, h or d
//! tag:baseline -tag:old       `-` or `not` negates a term
//! exp:mnist or exp:cifar      `or` between groups; terms in a group must all match
//! ```

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::model::{context_matches, Agg, Run, Summary};

/// A parsed query: groups joined by `or`, each a list of terms that must all match.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub groups: Vec<Vec<Term>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub negate: bool,
    pub kind: TermKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TermKind {
    /// A bare word: matched against name, experiment, tags, id and notes.
    Text(String),
    Compare {
        field: Field,
        op: Op,
        value: Lit,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Status,
    Experiment,
    Name,
    Tag,
    Id,
    Notes,
    Archived,
    /// Seconds from start to end (or now).
    Duration,
    /// Seconds since the run was created.
    Age,
    /// `training`, `eval` or another kind.
    Kind,
    /// The run's target reference: any of its values, or one key (`target.model`).
    Target(Option<String>),
    /// A parameter by its full dotted key.
    Param(String),
    /// A metric statistic.
    Metric {
        name: String,
        context: Option<String>,
        agg: Agg,
    },
    /// A bare identifier: a parameter key or its last segment, else a metric name.
    Auto(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Contains,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
}

/// Why a query could not be parsed, and where (a character offset).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParseError {
    pub message: String,
    pub position: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.position + 1)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Op(Op),
}

fn op_char(c: char) -> bool {
    matches!(c, '<' | '>' | '=' | '!' | ':')
}

fn tokenize(src: &str) -> Result<Vec<(usize, Tok)>, ParseError> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    // `i` already counts characters, so a position is `i` itself.
    let pos = |i: usize| i;
    while i < chars.len() {
        let (_, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i].1 != c {
                if chars[i].1 == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                s.push(chars[i].1);
                i += 1;
            }
            if i >= chars.len() {
                return Err(ParseError {
                    message: "this quote is never closed".into(),
                    position: pos(start),
                });
            }
            i += 1;
            out.push((pos(start), Tok::Str(s)));
        } else if op_char(c) {
            let start = i;
            let next = chars.get(i + 1).map(|x| x.1);
            let (op, len) = match (c, next) {
                ('<', Some('=')) => (Op::Le, 2),
                ('>', Some('=')) => (Op::Ge, 2),
                ('!', Some('=')) => (Op::Ne, 2),
                ('=', Some('=')) => (Op::Eq, 2),
                ('<', _) => (Op::Lt, 1),
                ('>', _) => (Op::Gt, 1),
                ('=', _) => (Op::Eq, 1),
                (':', _) => (Op::Contains, 1),
                _ => {
                    return Err(ParseError {
                        message: "`!` must be followed by `=`; use `-` or `not` to negate".into(),
                        position: pos(start),
                    })
                }
            };
            i += len;
            out.push((pos(start), Tok::Op(op)));
        } else {
            let start = i;
            let mut s = String::new();
            let mut depth = 0i32;
            while i < chars.len() {
                let ch = chars[i].1;
                if depth == 0 && (ch.is_whitespace() || op_char(ch) || ch == '"' || ch == '\'') {
                    break;
                }
                match ch {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
                s.push(ch);
                i += 1;
            }
            out.push((pos(start), Tok::Word(s)));
        }
    }
    Ok(out)
}

/// Parses a duration or number literal: `0.01`, `1e-3`, `10m`, `2h`, `3d`.
fn parse_number(s: &str) -> Option<f64> {
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    let unit = match s.chars().last()? {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        _ => return None,
    };
    s[..s.len() - 1].parse::<f64>().ok().map(|v| v * unit)
}

fn literal(t: &Tok) -> Lit {
    match t {
        Tok::Str(s) => Lit::Str(s.clone()),
        Tok::Word(w) => match w.to_ascii_lowercase().as_str() {
            "true" => Lit::Bool(true),
            "false" => Lit::Bool(false),
            "null" | "none" => Lit::Null,
            _ => parse_number(w).map_or_else(|| Lit::Str(w.clone()), Lit::Num),
        },
        Tok::Op(_) => Lit::Null,
    }
}

fn field(word: &str) -> Field {
    let lower = word.to_ascii_lowercase();
    match lower.as_str() {
        "status" | "state" => return Field::Status,
        "experiment" | "exp" => return Field::Experiment,
        "name" => return Field::Name,
        "tag" | "tags" => return Field::Tag,
        "id" | "run" => return Field::Id,
        "notes" | "description" => return Field::Notes,
        "archived" => return Field::Archived,
        "duration" => return Field::Duration,
        "age" => return Field::Age,
        "kind" => return Field::Kind,
        "target" => return Field::Target(None),
        _ => {}
    }
    if let Some(open) = word.find('[') {
        if let Some(close) = word.rfind(']') {
            if close > open {
                let name = word[..open].to_string();
                let context = Some(word[open + 1..close].to_string());
                let rest = &word[close + 1..];
                let agg = rest
                    .strip_prefix('.')
                    .and_then(Agg::parse)
                    .unwrap_or(Agg::Last);
                return Field::Metric { name, context, agg };
            }
        }
    }
    if let Some(key) = word.strip_prefix("target.") {
        return Field::Target(Some(key.to_string()));
    }
    if let Some(key) = word
        .strip_prefix("params.")
        .or_else(|| word.strip_prefix("p."))
    {
        return Field::Param(key.to_string());
    }
    Field::Auto(word.to_string())
}

impl Query {
    /// Parses `src`. An empty string is the query that matches every run.
    pub fn parse(src: &str) -> Result<Query, ParseError> {
        let toks = tokenize(src)?;
        let mut groups: Vec<Vec<Term>> = vec![Vec::new()];
        let mut i = 0;
        let mut negate = false;
        while i < toks.len() {
            let (pos, tok) = &toks[i];
            match tok {
                Tok::Word(w) if w.eq_ignore_ascii_case("or") => {
                    if groups.last().is_none_or(|g| g.is_empty()) || negate {
                        return Err(ParseError {
                            message: "`or` needs a term on both sides".into(),
                            position: *pos,
                        });
                    }
                    groups.push(Vec::new());
                    i += 1;
                    continue;
                }
                Tok::Word(w) if w.eq_ignore_ascii_case("and") => {
                    i += 1;
                    continue;
                }
                Tok::Word(w) if w.eq_ignore_ascii_case("not") => {
                    negate = !negate;
                    i += 1;
                    continue;
                }
                Tok::Op(_) => {
                    return Err(ParseError {
                        message: "a comparison needs a field before it, as in `lr<0.01`".into(),
                        position: *pos,
                    })
                }
                _ => {}
            }
            let (word, neg_prefix) = match tok {
                Tok::Word(w) if w.len() > 1 && w.starts_with('-') => (w[1..].to_string(), true),
                Tok::Word(w) => (w.clone(), false),
                Tok::Str(s) => (s.clone(), false),
                Tok::Op(_) => unreachable!(),
            };
            let term_negate = negate ^ neg_prefix;
            negate = false;
            let is_field = matches!(tok, Tok::Word(_));
            match toks.get(i + 1) {
                Some((op_pos, Tok::Op(op))) if is_field => {
                    let Some((_, v)) = toks.get(i + 2).filter(|(_, t)| !matches!(t, Tok::Op(_)))
                    else {
                        return Err(ParseError {
                            message: format!("`{word}` needs a value after the operator"),
                            position: *op_pos,
                        });
                    };
                    groups.last_mut().unwrap().push(Term {
                        negate: term_negate,
                        kind: TermKind::Compare {
                            field: field(&word),
                            op: *op,
                            value: literal(v),
                        },
                    });
                    i += 3;
                }
                _ => {
                    groups.last_mut().unwrap().push(Term {
                        negate: term_negate,
                        kind: TermKind::Text(word.to_lowercase()),
                    });
                    i += 1;
                }
            }
        }
        if negate {
            return Err(ParseError {
                message: "`not` needs a term after it".into(),
                position: src.chars().count(),
            });
        }
        if groups.len() > 1 && groups.last().is_some_and(|g| g.is_empty()) {
            return Err(ParseError {
                message: "`or` needs a term on both sides".into(),
                position: src.chars().count(),
            });
        }
        groups.retain(|g| !g.is_empty());
        Ok(Query { groups })
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether the run matches.
    pub fn matches(&self, r: &dyn RunView, now: i64) -> bool {
        self.groups.is_empty()
            || self
                .groups
                .iter()
                .any(|g| g.iter().all(|t| t.matches(r, now)))
    }
}

/// What a query needs to know about a run.
pub trait RunView {
    fn run(&self) -> &Run;
    /// Flattened parameters by dotted key.
    fn params(&self) -> &BTreeMap<String, Value>;
    /// Metric series: name, context and statistics.
    fn metrics(&self) -> Box<dyn Iterator<Item = (&str, &Value, &Summary)> + '_>;
}

/// A value a field resolves to.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl Val {
    pub fn from_json(v: &Value) -> Val {
        match v {
            Value::Null => Val::Null,
            Value::Bool(b) => Val::Bool(*b),
            Value::Number(n) => n.as_f64().map_or(Val::Null, Val::Num),
            Value::String(s) => Val::Str(s.clone()),
            other => Val::Str(other.to_string()),
        }
    }
}

/// Every scalar under `v` (`v` itself when it is not an object or array).
fn leaves(v: &Value, out: &mut Vec<Val>) {
    match v {
        Value::Object(m) => m.values().for_each(|x| leaves(x, out)),
        Value::Array(a) => a.iter().for_each(|x| leaves(x, out)),
        other => out.push(Val::from_json(other)),
    }
}

/// Resolves a field on a run: every value it can take (a run has several tags, a metric name
/// several contexts). A term matches when any value matches.
pub fn resolve(f: &Field, r: &dyn RunView, now: i64) -> Vec<Val> {
    let run = r.run();
    let opt = |s: &Option<String>| s.clone().map_or(Val::Null, Val::Str);
    match f {
        Field::Status => vec![Val::Str(run.status.as_str().into())],
        Field::Experiment => vec![opt(&run.experiment)],
        Field::Name => vec![Val::Str(run.display_name())],
        Field::Tag => run.tags.iter().cloned().map(Val::Str).collect(),
        Field::Id => vec![Val::Str(run.id.clone())],
        Field::Notes => vec![opt(&run.description)],
        Field::Archived => vec![Val::Bool(run.archived)],
        Field::Duration => vec![Val::Num(run.duration_secs(now))],
        Field::Age => vec![Val::Num(((now - run.created_at).max(0)) as f64 / 1e6)],
        Field::Kind => vec![Val::Str(run.kind().to_string())],
        Field::Target(key) => {
            let target = run.target();
            let node = match key {
                Some(k) => k.split('.').try_fold(&target, |v, part| v.get(part)),
                None => Some(&target),
            };
            let mut out = Vec::new();
            if let Some(v) = node {
                leaves(v, &mut out);
            }
            out
        }
        Field::Param(k) => r
            .params()
            .get(k)
            .map(|v| vec![Val::from_json(v)])
            .unwrap_or_default(),
        Field::Metric { name, context, agg } => r
            .metrics()
            .filter(|(n, c, _)| {
                n == name && context.as_deref().is_none_or(|f| context_matches(c, f))
            })
            .map(|(_, _, s)| agg.of(s).map_or(Val::Null, Val::Num))
            .collect(),
        Field::Auto(word) => {
            let params = r.params();
            if let Some(v) = params.get(word) {
                return vec![Val::from_json(v)];
            }
            let suffix = format!(".{word}");
            let vals: Vec<Val> = params
                .iter()
                .filter(|(k, _)| k.ends_with(&suffix))
                .map(|(_, v)| Val::from_json(v))
                .collect();
            if !vals.is_empty() {
                return vals;
            }
            resolve(
                &Field::Metric {
                    name: word.clone(),
                    context: None,
                    agg: Agg::Last,
                },
                r,
                now,
            )
        }
    }
}

fn compare(v: &Val, op: Op, lit: &Lit) -> bool {
    use std::cmp::Ordering;
    let ord: Option<Ordering> = match (v, lit) {
        (Val::Num(a), Lit::Num(b)) => a.partial_cmp(b),
        (Val::Str(a), Lit::Str(b)) => {
            if op == Op::Contains {
                return a.to_lowercase().contains(&b.to_lowercase());
            }
            Some(a.to_lowercase().cmp(&b.to_lowercase()))
        }
        (Val::Str(a), Lit::Num(b)) => match a.parse::<f64>() {
            Ok(x) => x.partial_cmp(b),
            Err(_) => {
                return match op {
                    Op::Ne => true,
                    Op::Contains => a.contains(&b.to_string()),
                    _ => false,
                }
            }
        },
        (Val::Num(a), Lit::Str(b)) => {
            return match op {
                Op::Contains => a.to_string().contains(b.as_str()),
                Op::Ne => true,
                _ => false,
            }
        }
        (Val::Bool(a), Lit::Bool(b)) => Some(a.cmp(b)),
        (Val::Bool(a), Lit::Str(b)) => Some(a.to_string().cmp(&b.to_lowercase())),
        (Val::Null, Lit::Null) => Some(Ordering::Equal),
        (_, Lit::Null) | (Val::Null, _) => return op == Op::Ne,
        _ => return op == Op::Ne,
    };
    let Some(o) = ord else { return false };
    match op {
        Op::Eq | Op::Contains => o == Ordering::Equal,
        Op::Ne => o != Ordering::Equal,
        Op::Lt => o == Ordering::Less,
        Op::Le => o != Ordering::Greater,
        Op::Gt => o == Ordering::Greater,
        Op::Ge => o != Ordering::Less,
    }
}

impl Term {
    fn matches(&self, r: &dyn RunView, now: i64) -> bool {
        let hit = match &self.kind {
            TermKind::Text(t) => {
                let run = r.run();
                run.display_name().to_lowercase().contains(t)
                    || run.id.to_lowercase().contains(t)
                    || run
                        .experiment
                        .as_deref()
                        .is_some_and(|e| e.to_lowercase().contains(t))
                    || run.tags.iter().any(|x| x.to_lowercase().contains(t))
                    || run
                        .description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(t))
            }
            TermKind::Compare { field, op, value } => {
                let vals = resolve(field, r, now);
                if vals.is_empty() {
                    // A missing field differs from everything and equals null.
                    matches!(op, Op::Ne) || (matches!(value, Lit::Null) && *op != Op::Ne)
                } else if *op == Op::Ne {
                    vals.iter().all(|v| compare(v, Op::Ne, value))
                } else {
                    vals.iter().any(|v| compare(v, *op, value))
                }
            }
        };
        hit != self.negate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Points, RunStatus};
    use serde_json::json;

    struct R {
        run: Run,
        params: BTreeMap<String, Value>,
        metrics: Vec<(String, Value, Summary)>,
    }

    impl RunView for R {
        fn run(&self) -> &Run {
            &self.run
        }
        fn params(&self) -> &BTreeMap<String, Value> {
            &self.params
        }
        fn metrics(&self) -> Box<dyn Iterator<Item = (&str, &Value, &Summary)> + '_> {
            Box::new(self.metrics.iter().map(|(n, c, s)| (n.as_str(), c, s)))
        }
    }

    fn summary(vals: &[f64]) -> Summary {
        Summary::of(&Points {
            steps: (0..vals.len() as i64).collect(),
            values: vals.to_vec(),
            epochs: vec![None; vals.len()],
            timestamps: vec![0; vals.len()],
        })
    }

    fn run() -> R {
        R {
            run: Run {
                id: "0193abcd".into(),
                name: Some("resnet-lr3".into()),
                experiment: Some("cifar10".into()),
                description: Some("Warmup was too short".into()),
                tags: vec!["baseline".into(), "sweep".into()],
                status: RunStatus::Failed,
                archived: false,
                created_at: 0,
                updated_at: 0,
                ended_at: Some(900_000_000),
                kind: Some("eval".into()),
                target: Some(json!({"model": "gpt-small-v2", "git": "4f2c1ab"})),
            },
            params: [
                ("hparams.lr".to_string(), json!(0.003)),
                ("hparams.batch_size".to_string(), json!(128)),
                ("optimizer.name".to_string(), json!("AdamW")),
                ("seed".to_string(), json!("7")),
            ]
            .into_iter()
            .collect(),
            metrics: vec![
                (
                    "loss".into(),
                    json!({"subset": "train"}),
                    summary(&[2.0, 0.5]),
                ),
                (
                    "loss".into(),
                    json!({"subset": "val"}),
                    summary(&[2.0, 0.2, 0.4]),
                ),
            ],
        }
    }

    fn check(q: &str) -> bool {
        Query::parse(q)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .matches(&run(), 3_600_000_000)
    }

    #[test]
    fn matches_fields_params_and_metrics() {
        for q in [
            "",
            "resnet",
            "RESNET cifar",
            "warmup",
            "0193",
            "status:failed",
            "status=FAILED",
            "exp:cifar",
            "tag:baseline",
            "-tag:old",
            "not tag:old",
            "lr<0.01",
            "hparams.lr=0.003",
            "params.hparams.lr>=0.003",
            "batch_size>64 lr<1e-2",
            "optimizer.name=adamw",
            "optimizer.name:adam",
            "seed=7",
            "loss[val]<0.5",
            "loss[val].min<0.25",
            "loss[subset=val].max>=2",
            "loss[train].count=2",
            "loss<0.45",
            "duration>10m duration<1h",
            "age>59m",
            "archived=false",
            "exp:mnist or tag:sweep",
            "missing!=3",
            "missing=null",
            "\"resnet-lr3\"",
            "kind:eval",
            "target:gpt-small",
            "target.git=4f2c1ab",
            "name='resnet-lr3'",
        ] {
            assert!(check(q), "{q} should match");
        }
        for q in [
            "mnist",
            "status:running",
            "-tag:baseline",
            "lr>0.01",
            "loss[val]<0.3",
            "loss[train].min<0.4",
            "missing=3",
            "exp:mnist or tag:old",
            "optimizer.name!=adamw",
            "kind=training",
            "target.model=other",
        ] {
            assert!(!check(q), "{q} should not match");
        }
    }

    #[test]
    fn parses_long_queries_in_linear_time() {
        let q = "a ".repeat(200_000);
        let t = std::time::Instant::now();
        assert_eq!(Query::parse(&q).unwrap().groups[0].len(), 200_000);
        assert!(t.elapsed().as_secs_f64() < 2.0, "{:?}", t.elapsed());
        assert_eq!(Query::parse("éé lr<").unwrap_err().position, 5);
    }

    #[test]
    fn reports_errors_with_positions() {
        let e = Query::parse("lr<").unwrap_err();
        assert_eq!(e.position, 2);
        assert!(Query::parse("<3").is_err());
        assert!(Query::parse("name=\"open").is_err());
        assert!(Query::parse("or a").is_err());
        assert!(Query::parse("a or").is_err());
        assert!(Query::parse("a ! b").is_err());
        assert_eq!(
            Query::parse("exp:a or exp:b").unwrap().groups.len(),
            2,
            "two groups"
        );
    }
}
