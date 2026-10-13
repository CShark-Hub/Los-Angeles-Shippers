// How the core reads a composed answer: the sentences, the fact ids each cites, and the text the output
// checks read and the answer delivers. The replay server matches the output stage's sentences with it, and
// run.mjs works out the answer a recording delivers. One module for both: the replay container mounts this
// folder, so server.mjs imports it from beside itself.
//
// Kept in step with the core, rule for rule: `sentences`, `citations`, `as_shown`, `without_citations`,
// `split`, `taken_apart`, `ids_inside`, `is_id`, `tidy` and `within_words` in mercury-core's
// `orchestrator/compose.rs`, and `without_invisible` in mercury-judge's `stages.rs`. Whitespace is Unicode's
// `White_Space`, as Rust's `\s`, `trim` and `split_whitespace` read it (JavaScript's own `\s` and `trim`
// differ by two characters). When the core's rules change, change these with them.

/** The most words an answer may have; sentences past it are left out. */
export const MAX_WORDS = 120;

/** What may close a sentence after its stop: a closing quote or bracket, the marks of bold or italic text, and the invisible characters. */
const CLOSERS = "[\"'”’»›)\\]}*_`\\p{Cf}\\p{Default_Ignorable_Code_Point}\\p{Variation_Selector}]*";
/** Brackets of nothing but fact ids, separators, spaces and more brackets: what may follow a stop and still belong to its sentence. */
const BRACKETED_IDS = "\\[(?:[\\p{White_Space},;\\[\\]]|f_[A-Za-z0-9]{4,16})*\\]";
/** A sentence's end: a stop, then what closes it and the citations written after it, before a space or the end. */
const END = new RegExp(
  `[.!?…]+${CLOSERS}(?:\\p{White_Space}*${BRACKETED_IDS}${CLOSERS})*(?:\\p{White_Space}+|$)`,
  "gu",
);
/** Characters that control, hide or reorder text, which the person doesn't read. */
const INVISIBLE = /[\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}\p{Variation_Selector}]/gu;
const SPACE = /^\p{White_Space}$/u;
const SPACES = /\p{White_Space}+/u;
const ID = /^f_[A-Za-z0-9]{4,16}$/;

const trim = (text) => text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
const trimStart = (text) => text.replace(/^\p{White_Space}+/u, "");

/** The text without its invisible characters: what a person reads of it. */
export const withoutInvisible = (text) => text.replace(INVISIBLE, "");

/** The text cut into sentences, each keeping the citations written after its stop. */
export function sentences(text) {
  const whole = trim(text);
  const found = [];
  let start = 0;
  for (const end of whole.matchAll(END)) {
    const stop = end.index + end[0].length;
    push(found, whole.slice(start, stop));
    start = stop;
  }
  push(found, whole.slice(start));
  return found;
}

function push(found, raw) {
  let piece = trim(raw);
  // citations that open a piece belong to the sentence before it
  if (found.length > 0) {
    for (let cited = leadingCitation(piece); cited !== null; cited = leadingCitation(piece)) {
      found[found.length - 1] += ` ${cited}`;
      piece = trimStart(piece.slice(cited.length));
    }
  }
  if (piece) found.push(piece);
}

/** The citation a piece opens with, nested brackets and all: a bracket that leaves no text once its citations are out. */
function leadingCitation(piece) {
  if (!piece.startsWith("[")) return null;
  let depth = 0;
  for (let at = 0; at < piece.length; at += 1) {
    if (piece[at] === "[") depth += 1;
    else if (piece[at] === "]") {
      depth -= 1;
      if (depth === 0) {
        const group = piece.slice(0, at + 1);
        return split(group)[0] === "" ? group : null;
      }
    }
  }
  return null;
}

/** The fact ids a sentence cites, in the order they are written, each once. */
export const citations = (sentence) => split(sentence)[1];

/** The sentence without its citations, as the person reads it. */
export const withoutCitations = (sentence) => split(sentence)[0];

/** A sentence as the output checks read it: the invisible characters gone, the citations out and the text tidied, then each cited id once. */
export function asShown(sentence) {
  const [text, ids] = split(withoutInvisible(sentence));
  return ids.length === 0 ? text : `${text} ${ids.map((id) => `[${id}]`).join(" ")}`;
}

/**
 * A sentence's text without its citations, tidied, and the ids they cite in the order they are written, each
 * once. A citation is a bracket of fact ids separated by commas, semicolons or spaces, or of nothing; brackets
 * inside one go first; anything else keeps the bracket in the text. Taken apart again until nothing changes.
 */
function split(sentence) {
  let [text, ids] = takenApart(sentence);
  for (;;) {
    const [again, more] = takenApart(text);
    for (const id of more) if (!ids.includes(id)) ids.push(id);
    if (again === text) return [text, ids];
    text = again;
  }
}

/** One pass of `split`: the brackets read left to right, each citation taken out when its bracket closes; then the text tidied. */
function takenApart(sentence) {
  // the text kept so far, each character with where it stands in the sentence
  const kept = [];
  // where each bracket that is still open starts in `kept`
  const open = [];
  const cited = [];
  let at = 0;
  for (const c of sentence) {
    if (c === "[") {
      open.push(kept.length);
      kept.push([at, c]);
    } else if (c === "]") {
      const start = open.pop();
      const ids = start === undefined ? null : idsInside(kept.slice(start + 1));
      if (ids) {
        cited.push(...ids);
        kept.length = start;
      } else {
        // a bracket around something else, or one that never opened: text
        kept.push([at, c]);
      }
    } else {
      kept.push([at, c]);
    }
    at += c.length;
  }
  cited.sort((a, b) => a[0] - b[0]);
  const ids = [];
  for (const [, id] of cited) if (!ids.includes(id)) ids.push(id);
  return [tidy(kept.map(([, c]) => c).join("")), ids];
}

/** The ids a bracket holds, each with where it stands, when it holds nothing but ids and their separators; `null` otherwise. */
function idsInside(inside) {
  const ids = [];
  let token = null;
  for (const [at, c] of inside) {
    if (SPACE.test(c) || c === "," || c === ";") {
      if (token) ids.push(token);
      token = null;
    } else {
      token ??= [at, ""];
      token[1] += c;
    }
  }
  if (token) ids.push(token);
  return ids.every(([, id]) => ID.test(id)) ? ids : null;
}

/** Spaces as one, and none before a stop or a comma a citation stood before. */
function tidy(text) {
  return text
    .split(SPACES)
    .filter(Boolean)
    .join(" ")
    .replaceAll(" .", ".")
    .replaceAll(" ,", ",")
    .replaceAll(" !", "!")
    .replaceAll(" ?", "?");
}

export const words = (text) => withoutCitations(text).split(SPACES).filter(Boolean).length;

/** The sentences that fit in `MAX_WORDS`, in order. */
export function withinWords(list) {
  let total = 0;
  const kept = [];
  for (const sentence of list) {
    total += words(sentence);
    if (total > MAX_WORDS) break;
    kept.push(sentence);
  }
  return kept;
}

/**
 * What the core makes of a composed answer when every id it cites is one the probe gave and the output checks
 * pass every sentence: `checked`, the sentences the output stage reads (as shown, the ids after each; a
 * sentence without text or without a citation is dropped, as the citation rule drops it); `text`, the answer
 * delivered (those sentences without their ids, joined by a space); `cited`, the facts it rests on, in order.
 */
export function answerOf(composed) {
  const checked = withinWords(sentences(composed))
    .map(asShown)
    .filter((sentence) => withoutCitations(sentence) !== "" && citations(sentence).length > 0);
  const cited = [];
  for (const sentence of checked) for (const id of citations(sentence)) if (!cited.includes(id)) cited.push(id);
  return { checked, text: checked.map(withoutCitations).join(" "), cited };
}
