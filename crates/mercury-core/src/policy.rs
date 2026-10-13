//! The effective policy and `authorize`: every "may this person ask this?" is decided here, by
//! pure functions over the configuration and the project's manifest, so a table test covers
//! every case and no handler decides on its own.

use mercury_schema::api::{RefusalCode, SignoffReason};
use mercury_schema::config::{Policies, RolePolicy};
use mercury_schema::manifest::{Deny, DenyTopic, JudgeContext, ProbeManifest};
use mercury_schema::redact::{RedactRule, RedactorError};
use mercury_schema::{ProjectId, QuestionClass, Redactor, Role};

/// A role's entry, or nothing for a role the file doesn't list: it may ask nothing.
struct Grants<'a> {
    classes: &'a [QuestionClass],
    signoff: &'a [QuestionClass],
    show_people: Option<bool>,
    aggregates_only: bool,
}

impl<'a> Grants<'a> {
    fn of(policies: &'a Policies, role: Role) -> Self {
        match policies.roles.get(&role) {
            Some(RolePolicy {
                classes,
                show_people,
                aggregates_only,
                signoff,
            }) => Self {
                classes,
                signoff,
                show_people: *show_people,
                aggregates_only: *aggregates_only,
            },
            None => Self {
                classes: &[],
                signoff: &[],
                show_people: None,
                aggregates_only: false,
            },
        }
    }
}

/// Who asks: their role and the projects they are granted (the pseudo-project `portfolio` may be
/// among them, for its approver flag).
#[derive(Debug, Clone, Copy)]
pub struct Caller<'a> {
    pub role: Role,
    pub granted: &'a [ProjectId],
}

/// What the question is about.
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    /// One project, with the manifest its probe registered or the copy in `projects.yaml`.
    Project {
        project: &'a ProjectId,
        manifest: &'a ProbeManifest,
    },
    /// Every project the caller is granted.
    Portfolio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
    /// Allowed, and the answer waits for an approver.
    NeedsSignoff(SignoffReason),
}

/// Why `authorize` refused, with what the hint needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// The project isn't granted. `granted` is what the caller may ask about instead.
    ForbiddenProject { granted: Vec<ProjectId> },
    /// The class isn't allowed here. `allowed` is what the caller may ask here instead.
    ForbiddenClass { allowed: Vec<QuestionClass> },
    /// The class is `other`.
    OutOfScope,
}

impl DenyReason {
    pub fn code(&self) -> RefusalCode {
        match self {
            DenyReason::ForbiddenProject { .. } => RefusalCode::ForbiddenProject,
            DenyReason::ForbiddenClass { .. } => RefusalCode::ForbiddenClass,
            DenyReason::OutOfScope => RefusalCode::OutOfScope,
        }
    }

    /// What the person is told they can do instead.
    pub fn hint(&self) -> String {
        match self {
            DenyReason::ForbiddenProject { granted } if granted.is_empty() => {
                "No project is granted to you yet; an admin can grant you one.".to_owned()
            }
            DenyReason::ForbiddenProject { granted } => {
                let names: Vec<&str> = granted.iter().map(ProjectId::as_str).collect();
                format!("You can ask about {}.", or_list(&names))
            }
            DenyReason::ForbiddenClass { allowed } if allowed.is_empty() => {
                "There is nothing you can ask about here.".to_owned()
            }
            DenyReason::ForbiddenClass { allowed } => {
                let names: Vec<&str> = allowed.iter().map(|c| hint_name(*c)).collect();
                format!("You can ask about {}.", or_list(&names))
            }
            DenyReason::OutOfScope => "Mercury answers questions about your projects: their progress, tests, blockers, health, recent changes and anything else about the work in them.".to_owned(),
        }
    }
}

fn or_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// A class as a hint names it: the wire name for the five a person knows by it, and words for `open`,
/// whose wire name means nothing to them.
fn hint_name(class: QuestionClass) -> &'static str {
    match class {
        QuestionClass::Open => "anything else about the project",
        other => class_name(other),
    }
}

/// A class's name as it travels.
pub fn class_name(class: QuestionClass) -> &'static str {
    match class {
        QuestionClass::Progress => "progress",
        QuestionClass::Tested => "tested",
        QuestionClass::Blocked => "blocked",
        QuestionClass::Health => "health",
        QuestionClass::Changes => "changes",
        QuestionClass::Open => "open",
        QuestionClass::Portfolio => "portfolio",
        QuestionClass::Forbidden => "forbidden",
        QuestionClass::Other => "other",
    }
}

/// What one role may do about one project, or across projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// In the order of [`QuestionClass`], so a list shown to the person is stable.
    pub allowed: Vec<QuestionClass>,
    /// What the input checks look for in every question.
    pub deny_topics: Vec<DenyTopic>,
    pub signoff: Vec<QuestionClass>,
    pub show_titles: bool,
    pub show_people: bool,
    pub show_evidence_refs: bool,
    pub aggregates_only: bool,
    /// Which repository texts the judge may see; empty across projects.
    pub judge_context: Vec<JudgeContext>,
}

const ALL_CLASSES: [QuestionClass; 9] = [
    QuestionClass::Progress,
    QuestionClass::Tested,
    QuestionClass::Blocked,
    QuestionClass::Health,
    QuestionClass::Changes,
    QuestionClass::Open,
    QuestionClass::Portfolio,
    QuestionClass::Forbidden,
    QuestionClass::Other,
];

fn denied_classes<'a>(deny: impl IntoIterator<Item = &'a Deny>) -> Vec<QuestionClass> {
    deny.into_iter()
        .filter_map(|d| match d {
            Deny::Class(class) => Some(*class),
            Deny::Topic(_) => None,
        })
        .collect()
}

fn denied_topics<'a>(deny: impl IntoIterator<Item = &'a Deny>) -> Vec<DenyTopic> {
    let mut topics = Vec::new();
    for d in deny {
        if let Deny::Topic(topic) = d
            && !topics.contains(topic)
        {
            topics.push(*topic);
        }
    }
    topics
}

impl EffectivePolicy {
    /// For a project: the role's classes that the manifest allows and neither list denies.
    pub fn for_project(policies: &Policies, role: Role, manifest: &ProbeManifest) -> Self {
        let r = Grants::of(policies, role);
        let deny = || policies.global.deny.iter().chain(&manifest.deny);
        let denied = denied_classes(deny());
        let allowed = ALL_CLASSES
            .into_iter()
            .filter(|c| QuestionClass::PROJECT.contains(c))
            .filter(|c| r.classes.contains(c) && manifest.allow.contains(c) && !denied.contains(c))
            .collect();
        let signoff = ALL_CLASSES
            .into_iter()
            .filter(|c| r.signoff.contains(c) || manifest.signoff.contains(c))
            .collect();
        Self {
            allowed,
            deny_topics: denied_topics(deny()),
            signoff,
            show_titles: policies.defaults.show_titles,
            show_people: r.show_people.unwrap_or(policies.defaults.show_people),
            show_evidence_refs: policies.defaults.show_evidence_refs,
            aggregates_only: r.aggregates_only,
            judge_context: manifest.judge_context.clone(),
        }
    }

    /// Across projects: `portfolio`, when the role has it and the global list doesn't deny it.
    pub fn for_portfolio(policies: &Policies, role: Role) -> Self {
        let r = Grants::of(policies, role);
        let portfolio = QuestionClass::Portfolio;
        let allowed = (r.classes.contains(&portfolio)
            && !denied_classes(&policies.global.deny).contains(&portfolio))
        .then_some(portfolio)
        .into_iter()
        .collect();
        let signoff = r.signoff.iter().copied().filter(|c| *c == portfolio).collect();
        Self {
            allowed,
            deny_topics: denied_topics(&policies.global.deny),
            signoff,
            show_titles: policies.defaults.show_titles,
            show_people: r.show_people.unwrap_or(policies.defaults.show_people),
            show_evidence_refs: policies.defaults.show_evidence_refs,
            aggregates_only: r.aggregates_only,
            judge_context: Vec::new(),
        }
    }

    /// Whether the role may ask across projects at all; a project's refusal hint names it.
    fn portfolio_allowed(policies: &Policies, role: Role) -> bool {
        !Self::for_portfolio(policies, role).allowed.is_empty()
    }
}

/// Decides one question, in the order the policy note gives: the project's grant first, so a
/// refusal never tells what another project allows; then `other`; then the class; then sign-off.
pub fn authorize(
    policies: &Policies,
    caller: Caller<'_>,
    scope: Scope<'_>,
    class: QuestionClass,
) -> Decision {
    let effective = match scope {
        Scope::Project { project, manifest } => {
            if !caller.granted.contains(project) {
                let granted = caller
                    .granted
                    .iter()
                    .filter(|p| p.as_str() != mercury_schema::config::PORTFOLIO)
                    .cloned()
                    .collect();
                return Decision::Deny(DenyReason::ForbiddenProject { granted });
            }
            EffectivePolicy::for_project(policies, caller.role, manifest)
        }
        Scope::Portfolio => EffectivePolicy::for_portfolio(policies, caller.role),
    };
    if class == QuestionClass::Other {
        return Decision::Deny(DenyReason::OutOfScope);
    }
    if !effective.allowed.contains(&class) {
        let mut allowed = effective.allowed;
        if matches!(scope, Scope::Project { .. }) && EffectivePolicy::portfolio_allowed(policies, caller.role)
        {
            allowed.push(QuestionClass::Portfolio);
        }
        return Decision::Deny(DenyReason::ForbiddenClass { allowed });
    }
    if effective.signoff.contains(&class) {
        Decision::NeedsSignoff(SignoffReason::Class)
    } else {
        Decision::Allow
    }
}

/// The manifest a project's policy reads when its probe registered one and the admin keeps a copy
/// in `projects.yaml`: the stricter wins, since the admin alone can only tighten. The redaction
/// rules, `deny` and sign-off are the union, the registered manifest's first and each entry once;
/// `judge_context` is the intersection; every other key is the registered manifest's, as the probe
/// reads it.
pub fn combine(registered: &ProbeManifest, copy: &ProbeManifest) -> ProbeManifest {
    fn union<T: Clone + PartialEq>(into: &mut Vec<T>, more: &[T]) {
        for item in more {
            if !into.contains(item) {
                into.push(item.clone());
            }
        }
    }
    let mut combined = registered.clone();
    union(&mut combined.redact, &copy.redact);
    union(&mut combined.deny, &copy.deny);
    union(&mut combined.signoff, &copy.signoff);
    combined
        .judge_context
        .retain(|text| copy.judge_context.contains(text));
    combined
}

/// The redactor of one project: the global rules, then the manifest's, compiled once and shared
/// by every path that stores or sends the project's text. E-mail addresses always go first.
pub fn project_redactor(policies: &Policies, manifest: &ProbeManifest) -> Result<Redactor, RedactorError> {
    let rules: Vec<RedactRule> = policies
        .global
        .redact
        .iter()
        .chain(&manifest.redact)
        .cloned()
        .collect();
    Redactor::new(&rules)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn read(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::read_to_string(root.join(path))
            .unwrap()
            .replace("\r\n", "\n")
    }

    fn policies() -> Policies {
        serde_norway::from_str(&read("config-examples/policies.yaml")).unwrap()
    }

    /// The fixture repository's manifest, as its probe registers it.
    fn delta() -> ProbeManifest {
        serde_norway::from_str(&read("tests/fixtures/repo-delta/.mercury/probe.yaml")).unwrap()
    }

    /// The same manifest with the rules the policy note's examples use: `tested` denied,
    /// `changes` held for sign-off, `health` not allowed.
    fn delta_strict() -> ProbeManifest {
        let text = read("tests/fixtures/repo-delta/.mercury/probe.yaml")
            .replace(
                "allow: [progress, tested, blocked, health, changes, open]",
                "allow: [progress, tested, blocked, changes]",
            )
            .replace("deny: []", "deny: [tested, people_evaluation]")
            .replace("signoff: []", "signoff: [changes]");
        serde_norway::from_str(&text).unwrap()
    }

    fn project(id: &str) -> ProjectId {
        ProjectId::parse(id).unwrap()
    }

    /// One letter per class, in the order of `ALL_CLASSES`: `A` allow, `S` sign-off,
    /// `C` forbidden_class, `P` forbidden_project, `O` out_of_scope.
    fn letters(policies: &Policies, role: Role, granted: &[ProjectId], scope: Scope<'_>) -> String {
        ALL_CLASSES
            .iter()
            .map(
                |class| match authorize(policies, Caller { role, granted }, scope, *class) {
                    Decision::Allow => 'A',
                    Decision::NeedsSignoff(_) => 'S',
                    Decision::Deny(DenyReason::ForbiddenClass { .. }) => 'C',
                    Decision::Deny(DenyReason::ForbiddenProject { .. }) => 'P',
                    Decision::Deny(DenyReason::OutOfScope) => 'O',
                },
            )
            .collect()
    }

    #[test]
    fn every_role_class_and_manifest_of_the_examples() {
        let policies = policies();
        let (delta, strict) = (delta(), delta_strict());
        let id = project("delta");
        let granted = [id.clone()];
        let elsewhere = [project("omega")];
        //            progress tested blocked health changes open portfolio forbidden other
        let table: [(Role, &str, &str, &str, &str); 4] = [
            //                 delta        strict       not granted  portfolio
            (Role::Pm, "AAAAAACCO", "ACACSCCCO", "PPPPPPPPP", "CCCCCCCCO"),
            (Role::TechLead, "AAAAAACCO", "ACACSCCCO", "PPPPPPPPP", "CCCCCCCCO"),
            (Role::Board, "CCCACCCCO", "CCCCCCCCO", "PPPPPPPPP", "CCCCCCSCO"),
            (Role::Admin, "CCCCCCCCO", "CCCCCCCCO", "PPPPPPPPP", "CCCCCCCCO"),
        ];
        let mut rows = 0;
        for (role, on_delta, on_strict, ungranted, across) in table {
            let delta_scope = Scope::Project {
                project: &id,
                manifest: &delta,
            };
            let strict_scope = Scope::Project {
                project: &id,
                manifest: &strict,
            };
            assert_eq!(
                letters(&policies, role, &granted, delta_scope),
                on_delta,
                "{role:?} on delta"
            );
            assert_eq!(
                letters(&policies, role, &granted, strict_scope),
                on_strict,
                "{role:?} on strict"
            );
            assert_eq!(
                letters(&policies, role, &elsewhere, delta_scope),
                ungranted,
                "{role:?} ungranted"
            );
            assert_eq!(
                letters(&policies, role, &elsewhere, strict_scope),
                ungranted,
                "{role:?} ungranted"
            );
            assert_eq!(
                letters(&policies, role, &granted, Scope::Portfolio),
                across,
                "{role:?} portfolio"
            );
            rows += 5 * ALL_CLASSES.len();
        }
        assert_eq!(
            rows, 180,
            "4 roles × 9 classes × (2 manifests × 2 grants + portfolio)"
        );
    }

    #[test]
    fn the_hints_say_what_is_allowed_instead() {
        let policies = policies();
        let manifest = delta();
        let id = project("delta");
        let granted = [id.clone(), project("portfolio")];
        let scope = Scope::Project {
            project: &id,
            manifest: &manifest,
        };
        let board = Caller {
            role: Role::Board,
            granted: &granted,
        };
        let Decision::Deny(reason) = authorize(&policies, board, scope, QuestionClass::Tested) else {
            panic!("board may not ask tested");
        };
        assert_eq!(reason.code(), RefusalCode::ForbiddenClass);
        assert_eq!(reason.hint(), "You can ask about health or portfolio.");
        let pm = Caller {
            role: Role::Pm,
            granted: &[project("gamma"), project("portfolio")],
        };
        let Decision::Deny(reason) = authorize(&policies, pm, scope, QuestionClass::Progress) else {
            panic!("delta isn't granted");
        };
        assert_eq!(reason.code(), RefusalCode::ForbiddenProject);
        assert_eq!(
            reason.hint(),
            "You can ask about gamma.",
            "never the pseudo-project"
        );
        let nobody = Caller {
            role: Role::Pm,
            granted: &[],
        };
        let Decision::Deny(reason) = authorize(&policies, nobody, scope, QuestionClass::Progress) else {
            panic!("nothing is granted");
        };
        assert!(reason.hint().contains("an admin can grant"));
        assert_eq!(
            DenyReason::OutOfScope.hint(),
            "Mercury answers questions about your projects: their progress, tests, blockers, health, recent changes and anything else about the work in them."
        );
    }

    #[test]
    fn a_hint_names_open_in_words_never_by_its_wire_name() {
        let policies = policies();
        let id = project("delta");
        let granted = [id.clone()];
        let pm = Caller {
            role: Role::Pm,
            granted: &granted,
        };
        // a project whose repository leaves `open` out: asking it is refused, and the hint doesn't offer it
        let strict = delta_strict();
        let scope = Scope::Project {
            project: &id,
            manifest: &strict,
        };
        let Decision::Deny(reason) = authorize(&policies, pm, scope, QuestionClass::Open) else {
            panic!("the stricter manifest leaves open out");
        };
        assert_eq!(reason.code(), RefusalCode::ForbiddenClass);
        assert_eq!(reason.hint(), "You can ask about progress, blocked or changes.");
        // one class denied, `open` still allowed: what is left is named, `open` in words
        let mut denied = delta();
        denied.deny = vec![Deny::Class(QuestionClass::Tested)];
        let scope = Scope::Project {
            project: &id,
            manifest: &denied,
        };
        let Decision::Deny(reason) = authorize(&policies, pm, scope, QuestionClass::Tested) else {
            panic!("tested is denied");
        };
        assert_eq!(
            reason.hint(),
            "You can ask about progress, blocked, health, changes or anything else about the project."
        );
        assert!(!reason.hint().contains("open"));
    }

    #[test]
    fn a_global_deny_takes_the_class_from_every_project() {
        let mut policies = policies();
        policies.global.deny.push(Deny::Class(QuestionClass::Changes));
        policies.global.deny.push(Deny::Class(QuestionClass::Portfolio));
        let id = project("delta");
        let manifest = delta();
        let scope = Scope::Project {
            project: &id,
            manifest: &manifest,
        };
        assert_eq!(
            letters(&policies, Role::Pm, std::slice::from_ref(&id), scope),
            "AAAACACCO"
        );
        assert_eq!(
            letters(&policies, Role::Board, &[], Scope::Portfolio),
            "CCCCCCCCO"
        );
    }

    #[test]
    fn the_effective_policy_carries_what_answers_show() {
        let policies = policies();
        let pm = EffectivePolicy::for_project(&policies, Role::Pm, &delta_strict());
        assert!(pm.show_people, "the pm role overrides the default");
        assert!(pm.show_titles && pm.show_evidence_refs && !pm.aggregates_only);
        assert_eq!(pm.signoff, [QuestionClass::Changes]);
        assert!(pm.deny_topics.contains(&DenyTopic::SourceCode), "global");
        assert!(
            pm.deny_topics.contains(&DenyTopic::PeopleEvaluation),
            "the manifest's"
        );
        assert_eq!(pm.judge_context, [JudgeContext::Titles, JudgeContext::Labels]);
        let board = EffectivePolicy::for_portfolio(&policies, Role::Board);
        assert!(board.aggregates_only && !board.show_people);
        assert_eq!(board.allowed, [QuestionClass::Portfolio]);
    }

    #[test]
    fn the_project_redactor_applies_the_global_and_the_manifest_rules() {
        let redactor = project_redactor(&policies(), &delta()).unwrap();
        let question = "Did Acme Corp sign off on the MegaBank export? Ask pm@example.com today.";
        assert_eq!(
            redactor.apply(question),
            "Did the client sign off on the the client export? Ask [email] today."
        );
    }

    /// The fixture's manifest with these four lists: client names to redact, what it denies, what
    /// needs sign-off, and what the judge may read.
    fn lists(
        clients: &[&str],
        deny: &[Deny],
        signoff: &[QuestionClass],
        judge: &[JudgeContext],
    ) -> ProbeManifest {
        let mut manifest = delta();
        manifest.redact = clients
            .iter()
            .map(|name| RedactRule {
                pattern: format!(r"(?i)\b{name}\b"),
                replace: "the client".to_owned(),
            })
            .collect();
        manifest.deny = deny.to_vec();
        manifest.signoff = signoff.to_vec();
        manifest.judge_context = judge.to_vec();
        manifest
    }

    #[test]
    fn the_admins_copy_and_a_registered_manifest_combine_the_stricter_winning() {
        use JudgeContext::{Labels, Titles};
        use QuestionClass::{Changes, Health, Tested};
        let (evaluation, code) = (
            Deny::Topic(DenyTopic::PeopleEvaluation),
            Deny::Topic(DenyTopic::SourceCode),
        );
        #[rustfmt::skip]
        let table: [(&str, ProbeManifest, ProbeManifest, ProbeManifest); 9] = [
            // what it shows,                  registered,                                   the admin's copy,                                       combined
            ("the same lists change nothing",  lists(&["megabank"], &[], &[], &[Titles, Labels]), lists(&["megabank"], &[], &[], &[Titles, Labels]), lists(&["megabank"], &[], &[], &[Titles, Labels])),
            ("redaction: the union",           lists(&["megabank"], &[], &[], &[Titles]),         lists(&["acme"], &[], &[], &[Titles]),             lists(&["megabank", "acme"], &[], &[], &[Titles])),
            ("redaction: a rule in both once", lists(&["megabank", "acme"], &[], &[], &[]),       lists(&["acme", "omega"], &[], &[], &[]),          lists(&["megabank", "acme", "omega"], &[], &[], &[])),
            ("deny: the union of classes",     lists(&[], &[], &[], &[]),                          lists(&[], &[Deny::Class(Tested)], &[], &[]),      lists(&[], &[Deny::Class(Tested)], &[], &[])),
            ("deny: the union of topics",      lists(&[], &[evaluation], &[], &[]),                lists(&[], &[code, evaluation], &[], &[]),         lists(&[], &[evaluation, code], &[], &[])),
            ("sign-off: the union",            lists(&[], &[], &[Changes], &[]),                   lists(&[], &[], &[Health, Changes], &[]),          lists(&[], &[], &[Changes, Health], &[])),
            ("judge context: the intersection", lists(&[], &[], &[], &[Titles, Labels]),          lists(&[], &[], &[], &[Labels]),                   lists(&[], &[], &[], &[Labels])),
            ("judge context: nothing in common", lists(&[], &[], &[], &[Titles]),                 lists(&[], &[], &[], &[Labels]),                   lists(&[], &[], &[], &[])),
            ("judge context: the copy can't widen", lists(&[], &[], &[], &[]),                    lists(&[], &[], &[], &[Titles, Labels]),           lists(&[], &[], &[], &[])),
        ];
        for (what, registered, copy, combined) in table {
            assert_eq!(combine(&registered, &copy), combined, "{what}");
        }
    }

    #[test]
    fn every_other_key_is_the_registered_manifests() {
        let registered = delta();
        let mut copy = delta();
        copy.allow = vec![QuestionClass::Progress];
        copy.module_labels.clear();
        copy.blocked_labels = vec!["on-hold".to_owned()];
        copy.exclude.clear();
        copy.display_name = "Something else".to_owned();
        let combined = combine(&registered, &copy);
        assert_eq!(combined, registered, "only the four lists combine");
        assert_eq!(
            combined.allow, registered.allow,
            "the admin narrows classes with deny"
        );
    }

    #[test]
    fn the_combined_manifest_is_what_authorize_and_the_redactor_read() {
        let policies = policies();
        let registered = delta();
        let mut copy = delta();
        copy.redact = vec![RedactRule {
            pattern: r"(?i)\bomega\s+bank\b".to_owned(),
            replace: "the client".to_owned(),
        }];
        copy.deny = vec![Deny::Class(QuestionClass::Tested)];
        copy.signoff = vec![QuestionClass::Changes];
        copy.judge_context = vec![JudgeContext::Labels];
        let combined = combine(&registered, &copy);
        let id = project("delta");
        let granted = [id.clone()];
        let alone = Scope::Project {
            project: &id,
            manifest: &registered,
        };
        let both = Scope::Project {
            project: &id,
            manifest: &combined,
        };
        //  progress tested blocked health changes open portfolio forbidden other
        assert_eq!(letters(&policies, Role::Pm, &granted, alone), "AAAAAACCO");
        assert_eq!(
            letters(&policies, Role::Pm, &granted, both),
            "ACAASACCO",
            "the copy's deny and sign-off hold on top of the registered manifest"
        );
        let redactor = project_redactor(&policies, &combined).unwrap();
        assert_eq!(
            redactor.apply("Acme, MegaBank and Omega Bank"),
            "the client, the client and the client",
            "the global rules, the registered manifest's and the copy's"
        );
        let effective = EffectivePolicy::for_project(&policies, Role::Pm, &combined);
        assert_eq!(effective.judge_context, [JudgeContext::Labels]);
    }
}
