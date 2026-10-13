//! The probes' replies on their way to the composer. Titles go first when the policy hides them;
//! then every repository text a model could read is checked, and a flagged one is withheld; then
//! each reply is cut into units the composer cites by id: a fact by its own id, a module's
//! figures, the build's health or a finding of the code reader by an id the core gives them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use mercury_schema::facts::{BuildStatus, Fact, ModuleStats, Tested};
use mercury_schema::ids::FieldPointer;
use mercury_schema::redact::{validate_safe_text, withheld_title};
use mercury_schema::{ModuleName, Redactor, SafeText, ToolName};
use serde_json::{Value, json};

use crate::probes::ToolReply;

/// What a withheld text reads in its place.
pub const WITHHELD: &str = "[withheld]";

/// A reply and what was withheld from it, by JSON Pointer.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The call's index in the plan.
    pub call: u16,
    pub tool: ToolName,
    pub reply: ToolReply,
    pub withheld: Vec<FieldPointer>,
}

/// How many items a reply holds, for its `tool_result`.
pub fn items(reply: &ToolReply) -> u32 {
    let n = match reply {
        ToolReply::Describe(r) => r.modules.len(),
        ToolReply::Progress(r) => r.items.len(),
        ToolReply::Blocked(r) => r.items.len(),
        ToolReply::Tested(r) => r.modules.len(),
        ToolReply::Health(_) => 1,
        ToolReply::Changes(r) => r.modules.len(),
        ToolReply::AskCode(r) => r.findings.len(),
    };
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// When a reply's data is from.
pub fn as_of(reply: &ToolReply) -> chrono::DateTime<chrono::Utc> {
    match reply {
        ToolReply::Describe(r) => r.snapshot.indexed_at,
        ToolReply::Progress(r) => r.as_of,
        ToolReply::Blocked(r) => r.as_of,
        ToolReply::Tested(r) => r.as_of,
        ToolReply::Health(r) => r.as_of,
        ToolReply::Changes(r) => r.as_of,
        ToolReply::AskCode(r) => r.as_of,
    }
}

/// Replaces every repository title with `Work item in <module>` before any model sees one, for a
/// policy with `show_titles: false`. A release has no module, so its name goes.
pub fn withhold_titles(reply: &mut ToolReply) {
    let facts = |items: &mut Vec<Fact>| {
        for fact in items {
            fact.title = withheld_title(&fact.module);
            fact.title_withheld = true;
        }
    };
    match reply {
        ToolReply::Progress(r) => facts(&mut r.items),
        ToolReply::Blocked(r) => facts(&mut r.items),
        ToolReply::Changes(r) => {
            for summary in &mut r.modules {
                let title = withheld_title(&summary.module);
                for notable in &mut summary.notable {
                    *notable = title.clone();
                }
            }
        }
        ToolReply::Health(r) => r.last_release.name = None,
        // a finding is the reader's line, not a repository title
        ToolReply::Describe(_) | ToolReply::Tested(_) | ToolReply::AskCode(_) => {}
    }
}

/// Every repository text of a reply, with its JSON Pointer: what the data checks read.
pub fn texts(reply: &ToolReply) -> Vec<(String, String)> {
    let mut texts = Vec::new();
    let mut facts = |items: &[Fact]| {
        for (i, fact) in items.iter().enumerate() {
            texts.push((format!("/items/{i}/title"), fact.title.as_str().to_owned()));
        }
    };
    match reply {
        ToolReply::Progress(r) => facts(&r.items),
        ToolReply::Blocked(r) => facts(&r.items),
        ToolReply::Changes(r) => {
            for (i, summary) in r.modules.iter().enumerate() {
                for (j, notable) in summary.notable.iter().enumerate() {
                    texts.push((format!("/modules/{i}/notable/{j}"), notable.as_str().to_owned()));
                }
            }
        }
        ToolReply::Health(r) => {
            if let Some(name) = &r.last_release.name {
                texts.push(("/last_release/name".to_owned(), name.as_str().to_owned()));
            }
        }
        ToolReply::Describe(r) => {
            texts.push(("/display_name".to_owned(), r.display_name.as_str().to_owned()))
        }
        ToolReply::AskCode(r) => {
            for (i, finding) in r.findings.iter().enumerate() {
                texts.push((format!("/findings/{i}/text"), finding.text.as_str().to_owned()));
            }
        }
        ToolReply::Tested(_) => {}
    }
    texts
}

#[derive(Debug, thiserror::Error)]
pub enum WithholdError {
    #[error("no text at {0}")]
    NoText(String),
    #[error("the reply doesn't hold its shape once a text is withheld")]
    Shape,
}

/// The reply with the texts at `pointers` read as [`WITHHELD`]. The reply is parsed back into its
/// type, so what comes out is as strict as what came in.
pub fn withhold(reply: &ToolReply, pointers: &[String]) -> Result<ToolReply, WithholdError> {
    let edits: Vec<(String, String)> = pointers
        .iter()
        .map(|pointer| (pointer.clone(), WITHHELD.to_owned()))
        .collect();
    rewrite(reply, &edits)
}

/// The reply with every repository text redacted by the project's rules, before anything reads it:
/// the probe redacts by the manifest's rules alone, and the core's own rules come on top here. A
/// text that no longer fits SafeText once redacted reads [`WITHHELD`], and its pointer comes back.
pub fn redact(reply: &ToolReply, redactor: &Redactor) -> Result<(ToolReply, Vec<String>), WithholdError> {
    let mut edits = Vec::new();
    let mut withheld = Vec::new();
    for (pointer, text) in texts(reply) {
        let redacted = redactor.apply(&text);
        if redacted == text {
            continue;
        }
        if validate_safe_text(&redacted).is_ok() {
            edits.push((pointer, redacted.into_owned()));
        } else {
            edits.push((pointer.clone(), WITHHELD.to_owned()));
            withheld.push(pointer);
        }
    }
    Ok((rewrite(reply, &edits)?, withheld))
}

/// The reply with the text at each pointer replaced, parsed back into its type.
fn rewrite(reply: &ToolReply, edits: &[(String, String)]) -> Result<ToolReply, WithholdError> {
    if edits.is_empty() {
        return Ok(reply.clone());
    }
    let (tool, mut value) = to_value(reply)?;
    for (pointer, new) in edits {
        match value.pointer_mut(pointer) {
            Some(text @ Value::String(_)) => *text = Value::String(new.clone()),
            _ => return Err(WithholdError::NoText(pointer.clone())),
        }
    }
    from_value(tool, value)
}

fn to_value(reply: &ToolReply) -> Result<(ToolName, Value), WithholdError> {
    let (tool, value) = match reply {
        ToolReply::Describe(r) => (ToolName::Describe, serde_json::to_value(r)),
        ToolReply::Progress(r) => (ToolName::Progress, serde_json::to_value(r)),
        ToolReply::Tested(r) => (ToolName::Tested, serde_json::to_value(r)),
        ToolReply::Blocked(r) => (ToolName::Blocked, serde_json::to_value(r)),
        ToolReply::Health(r) => (ToolName::Health, serde_json::to_value(r)),
        ToolReply::Changes(r) => (ToolName::Changes, serde_json::to_value(r)),
        ToolReply::AskCode(r) => (ToolName::AskCode, serde_json::to_value(r)),
    };
    Ok((tool, value.map_err(|_| WithholdError::Shape)?))
}

fn from_value(tool: ToolName, value: Value) -> Result<ToolReply, WithholdError> {
    let reply = match tool {
        ToolName::Describe => serde_json::from_value(value).map(ToolReply::Describe),
        ToolName::Progress => serde_json::from_value(value).map(ToolReply::Progress),
        ToolName::Tested => serde_json::from_value(value).map(ToolReply::Tested),
        ToolName::Blocked => serde_json::from_value(value).map(ToolReply::Blocked),
        ToolName::Health => serde_json::from_value(value).map(ToolReply::Health),
        ToolName::Changes => serde_json::from_value(value).map(ToolReply::Changes),
        ToolName::AskCode => serde_json::from_value(value).map(ToolReply::AskCode),
    };
    reply.map_err(|_| WithholdError::Shape)
}

/// What the person may see of a fact: people only when the policy shows them, the pull request
/// and issue numbers only when it shows evidence references.
#[derive(Debug, Clone, Copy)]
pub struct Shown {
    pub people: bool,
    pub evidence: bool,
}

impl Shown {
    pub fn fact(self, fact: &Fact) -> Fact {
        let mut fact = fact.clone();
        if !self.people {
            fact.people.clear();
        }
        if !self.evidence {
            for evidence in &mut fact.evidence {
                evidence.reference = None;
            }
        }
        fact
    }
}

/// One thing the composer may cite.
#[derive(Debug, Clone)]
pub struct Unit {
    pub id: String,
    /// What the composer reads, with `id` in it.
    pub json: Value,
    /// The fact behind it, for the answer's list; figures have none.
    pub fact: Option<Fact>,
    /// The fact's confidence; figures are counts, and count as certain.
    pub confidence: f64,
    /// For the template composer.
    pub kind: UnitKind,
    /// When figures were counted; a fact has its own dates.
    pub as_of: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub enum UnitKind {
    Fact,
    Tests(ModuleName),
    Health,
    Changes(ModuleName),
    /// A line the code reader reported. The composer may cite it, as what the reader said; it is no
    /// fact, so an answer never lists it among its facts, and the template composer writes nothing
    /// for it: no sentence of an answer is a model's but through the composer and the checks.
    Finding,
}

/// Whether a reply of the code reader lost a finding: the probe dropped one that failed the text
/// rules, or the core's redaction or the data checks withheld one.
pub fn lost_a_finding(reply: &Reply) -> bool {
    match &reply.reply {
        ToolReply::AskCode(r) => {
            r.withheld > 0
                || reply
                    .withheld
                    .iter()
                    .any(|p| p.as_str().starts_with("/findings/"))
        }
        _ => false,
    }
}

/// The replies cut into units, each with an id no other unit has. `modules` are the project's, as
/// `describe` lists them: a finding about another module keeps its text and loses the module, since
/// a module name the project doesn't have could carry an identifier.
pub fn units(replies: &[Reply], shown: Shown, modules: &[ModuleName]) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut taken: HashMap<String, ()> = HashMap::new();
    let mut fresh = |base: String| {
        let mut id = base;
        while taken.contains_key(&id) {
            id.push('x');
        }
        taken.insert(id.clone(), ());
        id
    };
    for reply in replies {
        let call = reply.call;
        match &reply.reply {
            ToolReply::Progress(r) => fact_units(&r.items, shown, &mut units, &mut fresh),
            ToolReply::Blocked(r) => fact_units(&r.items, shown, &mut units, &mut fresh),
            ToolReply::Tested(r) => {
                for (i, stats) in r.modules.iter().enumerate() {
                    let id = fresh(format!("f_c{call}t{i}"));
                    let mut json = serde_json::to_value(stats).unwrap_or(Value::Null);
                    tag(&mut json, &id, r.as_of.to_rfc3339());
                    // the verdict the probe gives a fact, read off the module's figures, so a model
                    // needn't work out that no tests at all is not "passing"
                    if let Some(object) = json.as_object_mut() {
                        let tested = serde_json::to_value(tested(stats)).unwrap_or(Value::Null);
                        object.insert("tested".to_owned(), tested);
                    }
                    units.push(figures(id, json, UnitKind::Tests(stats.module.clone()), r.as_of));
                }
            }
            ToolReply::Health(r) => {
                let id = fresh(format!("f_c{call}h0"));
                let mut json = serde_json::to_value(r).unwrap_or(Value::Null);
                if let Some(object) = json.as_object_mut() {
                    object.remove("schema");
                }
                tag(&mut json, &id, r.as_of.to_rfc3339());
                units.push(figures(id, json, UnitKind::Health, r.as_of));
            }
            ToolReply::Changes(r) => {
                for (i, summary) in r.modules.iter().enumerate() {
                    let id = fresh(format!("f_c{call}m{i}"));
                    let mut json = serde_json::to_value(summary).unwrap_or(Value::Null);
                    tag(&mut json, &id, r.as_of.to_rfc3339());
                    units.push(figures(
                        id,
                        json,
                        UnitKind::Changes(summary.module.clone()),
                        r.as_of,
                    ));
                }
            }
            ToolReply::AskCode(r) => {
                for (i, finding) in r.findings.iter().enumerate() {
                    // a finding a check withheld reads [withheld]: there is nothing to cite
                    if finding.text.as_str() == WITHHELD {
                        continue;
                    }
                    let id = fresh(format!("f_c{call}r{i}"));
                    let module = finding
                        .module
                        .as_ref()
                        .filter(|module| modules.contains(module))
                        .map(ModuleName::as_str);
                    let confidence = f64::from(finding.confidence) / 100.0;
                    units.push(Unit {
                        json: json!({
                            "id": id,
                            "source": "code_reader",
                            "text": finding.text.as_str(),
                            "module": module,
                            "confidence": confidence,
                            "as_of": r.as_of.to_rfc3339(),
                        }),
                        id,
                        fact: None,
                        confidence,
                        kind: UnitKind::Finding,
                        as_of: None,
                    });
                }
            }
            ToolReply::Describe(_) => {}
        }
    }
    units
}

fn fact_units(items: &[Fact], shown: Shown, units: &mut Vec<Unit>, fresh: &mut impl FnMut(String) -> String) {
    for fact in items {
        let fact = shown.fact(fact);
        // a fact's own id is what the answer lists it by; a clash with a figure's id is the figure's to avoid
        let id = fresh(fact.id.as_str().to_owned());
        let mut json = serde_json::to_value(&fact).unwrap_or(Value::Null);
        if let Some(object) = json.as_object_mut() {
            object.insert("id".to_owned(), Value::String(id.clone()));
        }
        units.push(Unit {
            id,
            confidence: fact.confidence.get(),
            json,
            fact: Some(fact),
            kind: UnitKind::Fact,
            as_of: None,
        });
    }
}

fn tag(json: &mut Value, id: &str, as_of: String) {
    if let Some(object) = json.as_object_mut() {
        object.insert("id".to_owned(), Value::String(id.to_owned()));
        object.insert("as_of".to_owned(), Value::String(as_of));
    }
}

fn figures(id: String, json: Value, kind: UnitKind, as_of: DateTime<Utc>) -> Unit {
    Unit {
        id,
        json,
        fact: None,
        confidence: 1.0,
        kind,
        as_of: Some(as_of),
    }
}

/// Whether a module is tested, by the probe's table: none counted is `missing` whatever CI says,
/// then the last CI run decides; tests that aren't counted are never `missing`.
pub fn tested(stats: &ModuleStats) -> Tested {
    match (stats.tests_total, stats.last_ci.status) {
        (Some(0), _) => Tested::Missing,
        (_, BuildStatus::Failed) => Tested::Failing,
        (_, BuildStatus::Passed) => Tested::Passing,
        _ => Tested::Unknown,
    }
}

/// What the composer reads: the units, nothing else.
pub fn facts_json(units: &[Unit]) -> Value {
    json!(units.iter().map(|unit| unit.json.clone()).collect::<Vec<_>>())
}

/// A withheld value, as a [`SafeText`].
pub fn withheld_text() -> Option<SafeText> {
    serde_json::from_value(Value::String(WITHHELD.to_owned())).ok()
}

#[cfg(test)]
mod tests {
    use mercury_schema::facts::{ChangesReply, HealthReply, ProgressReply};

    use super::*;

    fn fact(id: &str, title: &str, module: &str) -> Value {
        json!({ "id": id, "kind": "feature", "title": title, "module": module, "status": "done",
                "tested": "passing", "confidence": 0.8, "waiting_on": null, "people": ["Dana Reyes"],
                "evidence": [{ "type": "pr", "ref": "#412", "state": "merged", "at": "2026-10-01" }],
                "title_withheld": false, "updated_at": "2026-10-01T10:00:00Z" })
    }

    fn progress() -> ToolReply {
        ToolReply::Progress(
            serde_json::from_value::<ProgressReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-02T08:00:00Z",
                "items": [fact("f_7Qx2", "Export invoices to CSV", "Export"), fact("f_9Kd4", "Fix invoice numbering", "Invoicing")]
            }))
            .unwrap(),
        )
    }

    fn changes() -> ToolReply {
        ToolReply::Changes(
            serde_json::from_value::<ChangesReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-02T08:00:00Z",
                "modules": [{ "module": "Export", "commits": 4, "merged_prs": 2,
                              "notable": ["Export invoices to CSV", "Faster export"] }]
            }))
            .unwrap(),
        )
    }

    #[test]
    fn hidden_titles_become_the_modules_work_items() {
        let mut reply = progress();
        withhold_titles(&mut reply);
        let ToolReply::Progress(r) = &reply else { panic!() };
        assert_eq!(r.items[0].title.as_str(), "Work item in Export");
        assert!(r.items.iter().all(|f| f.title_withheld));
        let mut reply = changes();
        withhold_titles(&mut reply);
        assert_eq!(texts(&reply)[1].1, "Work item in Export");
    }

    #[test]
    fn a_flagged_text_is_withheld_and_the_reply_stays_strict() {
        let reply = progress();
        let found = texts(&reply);
        assert_eq!(
            found[1],
            ("/items/1/title".to_owned(), "Fix invoice numbering".to_owned())
        );
        let withheld = withhold(&reply, &["/items/1/title".to_owned()]).unwrap();
        let ToolReply::Progress(r) = &withheld else {
            panic!()
        };
        assert_eq!(r.items[1].title.as_str(), WITHHELD);
        assert_eq!(r.items[0].title.as_str(), "Export invoices to CSV");
        assert!(withhold(&reply, &["/items/9/title".to_owned()]).is_err());
        assert!(withheld_text().is_some(), "[withheld] is itself a SafeText");
    }

    #[test]
    fn units_cite_facts_by_their_ids_and_figures_by_new_ones() {
        let replies = [
            Reply {
                call: 0,
                tool: ToolName::Progress,
                reply: progress(),
                withheld: vec![],
            },
            Reply {
                call: 1,
                tool: ToolName::Changes,
                reply: changes(),
                withheld: vec![],
            },
        ];
        let units = units(
            &replies,
            Shown {
                people: false,
                evidence: true,
            },
            &modules(),
        );
        let ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["f_7Qx2", "f_9Kd4", "f_c1m0"]);
        assert!(units.iter().all(|u| mercury_schema::FactId::parse(&u.id).is_ok()));
        let first = units[0].fact.as_ref().unwrap();
        assert!(first.people.is_empty(), "people only when the policy shows them");
        assert_eq!(units[0].json["people"], json!([]));
        assert_eq!(units[2].json["as_of"], "2026-10-02T08:00:00+00:00");
        assert_eq!(units[2].confidence, 1.0);
        let hidden = super::units(
            &replies[..1],
            Shown {
                people: true,
                evidence: false,
            },
            &modules(),
        );
        assert!(
            hidden[0]
                .fact
                .as_ref()
                .unwrap()
                .evidence
                .iter()
                .all(|e| e.reference.is_none())
        );
    }

    #[test]
    fn a_modules_figures_carry_its_tested_verdict_and_when_they_were_counted() {
        use mercury_schema::facts::TestedReply;
        let reply = ToolReply::Tested(
            serde_json::from_value::<TestedReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-02T08:00:00Z", "modules": [
                    { "module": "Auth", "tests_total": 0, "tests_changed_14d": 0,
                      "last_ci": { "status": "passed", "at": "2026-10-02T07:00:00Z" }, "pass_rate_7d": 1.0, "flaky_7d": 0.0 },
                    { "module": "Export", "tests_total": 9, "tests_changed_14d": 1,
                      "last_ci": { "status": "failed", "at": "2026-09-29T08:09:41Z" }, "pass_rate_7d": 0.5, "flaky_7d": 0.0 },
                    { "module": "Billing", "tests_total": null, "tests_changed_14d": null,
                      "last_ci": { "status": "running", "at": null }, "pass_rate_7d": 0.5, "flaky_7d": 0.0 }
                ]
            }))
            .unwrap(),
        );
        let replies = [Reply {
            call: 0,
            tool: ToolName::Tested,
            reply,
            withheld: vec![],
        }];
        let units = units(
            &replies,
            Shown {
                people: false,
                evidence: false,
            },
            &modules(),
        );
        let verdicts: Vec<&Value> = units.iter().map(|u| &u.json["tested"]).collect();
        assert_eq!(
            verdicts,
            [&json!("missing"), &json!("failing"), &json!("unknown")]
        );
        assert_eq!(units[1].json["last_ci"]["at"], "2026-09-29T08:09:41Z");
        assert!(
            units
                .iter()
                .all(|u| u.as_of.map(|at| at.to_rfc3339()).as_deref() == Some("2026-10-02T08:00:00+00:00"))
        );
    }

    #[test]
    fn a_release_name_is_a_text_to_check() {
        let reply = ToolReply::Health(
            serde_json::from_value::<HealthReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-02T08:00:00Z",
                "build": { "status": "passed", "at": "2026-10-02T07:00:00Z" }, "ci_pass_rate_7d": 0.9,
                "open_prs": { "count": 3, "oldest_days": 5 }, "todo_fixme": null,
                "last_release": { "name": "Release two", "at": "2026-09-30T12:00:00Z" }, "score": 82
            }))
            .unwrap(),
        );
        assert_eq!(
            texts(&reply),
            [("/last_release/name".to_owned(), "Release two".to_owned())]
        );
        assert_eq!(items(&reply), 1);
    }

    #[test]
    fn every_finding_is_a_text_to_check() {
        use mercury_schema::facts::AskCodeReply;
        let reply = ToolReply::AskCode(
            serde_json::from_value::<AskCodeReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-04T15:12:30Z",
                "findings": [
                    { "text": "The export writes one file per invoice batch", "module": "Export", "confidence": 82 },
                    { "text": "Totals are rounded once, after the discounts", "module": null, "confidence": 64 }
                ],
                "withheld": 1, "cost_usd": 0.0031, "prompt_version": 1
            }))
            .unwrap(),
        );
        assert_eq!(items(&reply), 2);
        assert_eq!(as_of(&reply).to_rfc3339(), "2026-10-04T15:12:30+00:00");
        let found = texts(&reply);
        assert_eq!(
            found,
            [
                (
                    "/findings/0/text".to_owned(),
                    "The export writes one file per invoice batch".to_owned()
                ),
                (
                    "/findings/1/text".to_owned(),
                    "Totals are rounded once, after the discounts".to_owned()
                ),
            ]
        );
        let withheld = withhold(&reply, &["/findings/1/text".to_owned()]).unwrap();
        let ToolReply::AskCode(r) = &withheld else {
            panic!()
        };
        assert_eq!(r.findings[1].text.as_str(), WITHHELD);
        assert_eq!(
            r.findings[0].text.as_str(),
            "The export writes one file per invoice batch"
        );
        let mut kept = reply.clone();
        withhold_titles(&mut kept);
        assert_eq!(kept, reply, "a finding is no title");
        let replies = [Reply {
            call: 0,
            tool: ToolName::AskCode,
            reply,
            withheld: vec![],
        }];
        let shown = Shown {
            people: true,
            evidence: true,
        };
        let units = units(&replies, shown, &modules());
        assert_eq!(units.len(), 2, "each finding is a unit");
        assert!(
            units.iter().all(|unit| unit.fact.is_none()),
            "a finding is no fact"
        );
    }

    fn modules() -> Vec<ModuleName> {
        ["Export", "Invoicing", "Other"]
            .iter()
            .map(|name| ModuleName::parse(name).unwrap())
            .collect()
    }

    /// A reply of the code reader with these findings, each a text, a module and a confidence.
    fn reader(findings: &[(&str, Option<&str>, u8)]) -> ToolReply {
        use mercury_schema::facts::AskCodeReply;
        let findings: Vec<Value> = findings
            .iter()
            .map(|(text, module, confidence)| json!({ "text": text, "module": module, "confidence": confidence }))
            .collect();
        ToolReply::AskCode(
            serde_json::from_value::<AskCodeReply>(json!({
                "schema": "mercury.facts/0.1", "as_of": "2026-10-04T15:12:30Z",
                "findings": findings, "withheld": 0, "cost_usd": 0.0031, "prompt_version": 1
            }))
            .unwrap(),
        )
    }

    fn replies_of(reply: ToolReply, withheld: &[&str]) -> Vec<Reply> {
        vec![Reply {
            call: 2,
            tool: ToolName::AskCode,
            reply,
            withheld: withheld.iter().map(|p| FieldPointer::parse(p).unwrap()).collect(),
        }]
    }

    const SHOWN: Shown = Shown {
        people: false,
        evidence: false,
    };

    #[test]
    fn findings_become_units_with_their_confidence() {
        let reply = reader(&[
            ("The export writes one file per invoice batch", Some("Export"), 82),
            ("Totals are rounded once, after the discounts", None, 64),
        ]);
        let units = units(&replies_of(reply, &[]), SHOWN, &modules());
        let ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["f_c2r0", "f_c2r1"]);
        assert!(
            units.iter().all(|u| mercury_schema::FactId::parse(&u.id).is_ok()),
            "an id the citation pattern knows"
        );
        for unit in &units {
            assert!(matches!(unit.kind, UnitKind::Finding));
            assert!(unit.fact.is_none(), "an answer's facts never list a finding");
            assert!(unit.as_of.is_none(), "figures are counted, a finding is not");
        }
        assert_eq!(units[0].confidence, 0.82);
        assert_eq!(units[1].confidence, 0.64);
        assert_eq!(
            units[0].json,
            json!({
                "id": "f_c2r0", "source": "code_reader",
                "text": "The export writes one file per invoice batch",
                "module": "Export", "confidence": 0.82, "as_of": "2026-10-04T15:12:30+00:00"
            })
        );
        assert_eq!(units[1].json["module"], Value::Null);
    }

    #[test]
    fn a_withheld_finding_is_no_unit() {
        let reply = reader(&[
            ("The export writes one file per invoice batch", Some("Export"), 82),
            ("Totals are rounded once, after the discounts", None, 64),
        ]);
        let withheld = withhold(&reply, &["/findings/0/text".to_owned()]).unwrap();
        let replies = replies_of(withheld, &["/findings/0/text"]);
        let left = units(&replies, SHOWN, &modules());
        let ids: Vec<&str> = left.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, ["f_c2r1"], "the other keeps the id its place gave it");
        // every one withheld: nothing for the composer to cite, and the reply says it lost findings
        let all = withhold(
            &reply,
            &["/findings/0/text".to_owned(), "/findings/1/text".to_owned()],
        )
        .unwrap();
        let replies = replies_of(all, &["/findings/0/text", "/findings/1/text"]);
        assert!(units(&replies, SHOWN, &modules()).is_empty());
        assert!(lost_a_finding(&replies[0]));
        // the probe's own count of what it dropped is a loss too, and a reply that lost none is none
        assert!(!lost_a_finding(
            &replies_of(reader(&[("A line", None, 70)]), &[])[0]
        ));
        let mut dropped = replies_of(reader(&[("A line", None, 70)]), &[]);
        if let ToolReply::AskCode(r) = &mut dropped[0].reply {
            r.withheld = 2;
        }
        assert!(lost_a_finding(&dropped[0]));
        assert!(!lost_a_finding(&Reply {
            call: 0,
            tool: ToolName::Progress,
            reply: progress(),
            withheld: vec![FieldPointer::parse("/items/0/title").unwrap()],
        }));
    }

    #[test]
    fn a_findings_module_the_manifest_lacks_is_dropped() {
        let reply = reader(&[
            ("The pay run reads the rates table", Some("Payroll"), 71),
            ("The export writes one file per invoice batch", Some("Export"), 82),
        ]);
        let units = units(&replies_of(reply, &[]), SHOWN, &modules());
        assert_eq!(
            units[0].json["module"],
            Value::Null,
            "a name the project doesn't have"
        );
        assert_eq!(
            units[0].json["text"], "The pay run reads the rates table",
            "the text stays"
        );
        assert_eq!(units[1].json["module"], "Export");
    }

    #[test]
    fn every_finding_is_redacted_and_read_by_the_data_checks() {
        use mercury_schema::redact::RedactRule;
        let reply = reader(&[
            ("Megabank is billed once a month", Some("Invoicing"), 77),
            ("Totals are rounded once", None, 64),
        ]);
        // what the data checks read: every finding's text, by its pointer
        assert_eq!(
            texts(&reply)
                .iter()
                .map(|(pointer, _)| pointer.as_str())
                .collect::<Vec<_>>(),
            ["/findings/0/text", "/findings/1/text"]
        );
        // the project's rules come on top of the probe's
        let client = Redactor::new(&[RedactRule {
            pattern: r"(?i)\bmegabank\b".to_owned(),
            replace: "the client".to_owned(),
        }])
        .unwrap();
        let (redacted, withheld) = redact(&reply, &client).unwrap();
        assert!(withheld.is_empty());
        let ToolReply::AskCode(r) = &redacted else {
            panic!()
        };
        assert_eq!(r.findings[0].text.as_str(), "the client is billed once a month");
        assert_eq!(
            r.findings[1].text.as_str(),
            "Totals are rounded once",
            "an untouched text stays"
        );
        // a text that no longer fits the shape rules once redacted is withheld, not shown
        let path = Redactor::new(&[RedactRule {
            pattern: r"(?i)\bmegabank\b".to_owned(),
            replace: "src/Billing/Invoice.cs".to_owned(),
        }])
        .unwrap();
        let (redacted, withheld) = redact(&reply, &path).unwrap();
        assert_eq!(withheld, ["/findings/0/text"]);
        let ToolReply::AskCode(r) = &redacted else {
            panic!()
        };
        assert_eq!(r.findings[0].text.as_str(), WITHHELD);
    }
}
