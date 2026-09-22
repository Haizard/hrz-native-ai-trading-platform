/*
 * The visual builder: a form over one StrategyDocument.
 *
 * docs/14 asks for three editor modes sharing one document. This is the third,
 * and it is deliberately the thinnest possible thing that is still honest:
 *
 *   - it knows nothing about the vocabulary. Every field, function, operator
 *     and stop rule comes from `GET /strategies/schema`, which is generated
 *     from `strategy-dsl` itself. A second hand-written list would drift, and
 *     drift here means offering a condition the validator then rejects.
 *   - it never parses YAML. Loading a document goes through
 *     `POST /strategies/validate`, whose response echoes the document the real
 *     parser produced. There is exactly one opinion about what a document
 *     means, and it is in Rust.
 *
 * The one thing it does parse is a *condition*, because a dropdown cannot
 * represent `close > vwap` without taking it apart. That parser is a subset of
 * the grammar in `expr.rs`, and anything it does not model degrades to a raw
 * text clause rather than to a wrong strategy -- so a grammar change the
 * builder has not caught up with shows the condition as text instead of
 * silently editing it.
 *
 * Nothing here touches the DOM or the network, and nothing here does
 * arithmetic over market data: it writes numbers a human typed into a
 * document, which is not the same thing. That is why it can run in Node, and
 * `tools/check_builder.mjs` does exactly that.
 */
(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.StrategyBuilder = api;
})(typeof globalThis === "object" ? globalThis : this, function () {
  "use strict";

  /// A number, as `expr.rs` lexes one. Used to reject a value that would be
  /// written bare and then read back as a string.
  const NUMBER = /^-?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;

  /// Operator symbols, longest first so `>=` is not read as `>` then `=`.
  const OPERATOR_SYMBOLS = ["==", "!=", ">=", "<=", ">", "<"];

  const GROUP_KEYS = ["entryAll", "entryAny", "invalidation", "exitAll", "exitAny"];

  /// What each group means, for the UI and for the messages it shows.
  const GROUPS = {
    entryAll: { label: "Enter when all of these hold", path: "entry.all_of" },
    entryAny: { label: "Enter when any of these hold", path: "entry.any_of" },
    invalidation: { label: "The setup is void when", path: "invalidation" },
    exitAll: { label: "Exit early when all of these hold", path: "exit.all_of" },
    exitAny: { label: "Exit early when any of these hold", path: "exit.any_of" },
  };

  // -------------------------------------------------------------------------
  // Operands and clauses
  // -------------------------------------------------------------------------

  function escapeDouble(text) {
    return text.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  }

  /// One operand as it appears in a condition.
  function operandText(operand) {
    if (!operand) return "";
    switch (operand.kind) {
      case "number":
      case "field":
        return String(operand.value);
      case "bool":
        return operand.value ? "true" : "false";
      case "string":
        return `"${escapeDouble(String(operand.value))}"`;
      case "call":
        return `${operand.func}(${(operand.args || []).map(operandText).join(", ")})`;
      default:
        return "";
    }
  }

  /// One clause as it appears in a condition.
  ///
  /// `not` binds looser than a comparison in the grammar, so `not close > vwap`
  /// means `not (close > vwap)` and needs no parentheses.
  function clauseText(clause) {
    if (!clause) return "";
    let text;
    if (clause.form === "raw") {
      text = String(clause.text || "");
    } else if (clause.form === "call") {
      text = `${clause.func}(${(clause.args || []).map(operandText).join(", ")})`;
    } else {
      const left = operandText(clause.left);
      if (!clause.op) {
        text = left;
      } else {
        let right = operandText(clause.right);
        // `threshold(x)` is how a document marks a number as tunable.
        if (clause.tunable && clause.right && clause.right.kind === "number") {
          right = `threshold(${right})`;
        }
        text = `${left} ${clause.op} ${right}`;
      }
    }
    return clause.not ? `not ${text}` : text;
  }

  /// One row as it appears in a document: its clauses joined by one connective.
  function rowText(row) {
    const parts = (row.clauses || []).map(clauseText).filter(Boolean);
    if (!parts.length) return "";
    return parts.join(row.joiner === "or" ? " or " : " and ");
  }

  // -------------------------------------------------------------------------
  // Reading a condition back apart
  // -------------------------------------------------------------------------

  function tokenize(text) {
    const tokens = [];
    let i = 0;
    while (i < text.length) {
      const c = text[i];
      if (c === " " || c === "\t" || c === "\n" || c === "\r") {
        i += 1;
        continue;
      }
      if (c === "(" || c === ")" || c === ",") {
        tokens.push({ type: c });
        i += 1;
        continue;
      }
      if (c === '"' || c === "'") {
        let value = "";
        let j = i + 1;
        while (j < text.length && text[j] !== c) {
          value += text[j];
          j += 1;
        }
        if (j >= text.length) return null;
        tokens.push({ type: "string", value });
        i = j + 1;
        continue;
      }
      const two = text.slice(i, i + 2);
      if (OPERATOR_SYMBOLS.indexOf(two) >= 0) {
        tokens.push({ type: "op", value: two });
        i += 2;
        continue;
      }
      if (c === ">" || c === "<") {
        tokens.push({ type: "op", value: c });
        i += 1;
        continue;
      }
      if (/[0-9]/.test(c) || (c === "-" && /[0-9.]/.test(text[i + 1] || ""))) {
        const match = /^-?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?/.exec(text.slice(i));
        if (!match) return null;
        tokens.push({ type: "number", value: match[0] });
        i += match[0].length;
        continue;
      }
      if (/[A-Za-z_]/.test(c)) {
        const match = /^[A-Za-z_][A-Za-z0-9_.]*/.exec(text.slice(i));
        tokens.push({ type: "ident", value: match[0] });
        i += match[0].length;
        continue;
      }
      return null;
    }
    return tokens;
  }

  /// Parse a condition into the grammar's own shape.
  ///
  /// Returns `null` for anything it cannot model. That is the important part:
  /// the caller then keeps the text verbatim instead of guessing.
  function parseCondition(text) {
    const tokens = tokenize(text);
    if (!tokens) return null;
    let pos = 0;

    const peek = () => tokens[pos];
    const eat = (type, value) => {
      const t = tokens[pos];
      if (!t || t.type !== type) return false;
      if (value !== undefined && t.value !== value) return false;
      pos += 1;
      return true;
    };

    function parsePrimary() {
      const t = peek();
      if (!t) return null;
      if (t.type === "number") {
        pos += 1;
        return { t: "lit", kind: "number", value: t.value };
      }
      if (t.type === "string") {
        pos += 1;
        return { t: "lit", kind: "string", value: t.value };
      }
      if (t.type === "(") {
        pos += 1;
        const inner = parseOr();
        if (!inner || !eat(")")) return null;
        return inner;
      }
      if (t.type === "ident") {
        if (t.value === "true" || t.value === "false") {
          pos += 1;
          return { t: "lit", kind: "bool", value: t.value === "true" };
        }
        pos += 1;
        if (peek() && peek().type === "(") {
          pos += 1;
          const args = [];
          if (peek() && peek().type === ")") {
            pos += 1;
          } else {
            for (;;) {
              const arg = parseOr();
              if (!arg) return null;
              args.push(arg);
              if (eat(",")) continue;
              if (eat(")")) break;
              return null;
            }
          }
          return { t: "call", name: t.value, args };
        }
        return { t: "field", name: t.value };
      }
      return null;
    }

    function parseComparison() {
      const left = parsePrimary();
      if (!left) return null;
      const t = peek();
      if (t && t.type === "op") {
        pos += 1;
        const right = parsePrimary();
        if (!right) return null;
        return { t: "cmp", op: t.value, left, right };
      }
      return left;
    }

    function parseNot() {
      const t = peek();
      if (t && t.type === "ident" && t.value === "not") {
        pos += 1;
        const inner = parseNot();
        if (!inner) return null;
        return { t: "not", e: inner };
      }
      return parseComparison();
    }

    function parseAnd() {
      const first = parseNot();
      if (!first) return null;
      const items = [first];
      while (peek() && peek().type === "ident" && peek().value === "and") {
        pos += 1;
        const next = parseNot();
        if (!next) return null;
        items.push(next);
      }
      return items.length === 1 ? items[0] : { t: "and", items };
    }

    function parseOr() {
      const first = parseAnd();
      if (!first) return null;
      const items = [first];
      while (peek() && peek().type === "ident" && peek().value === "or") {
        pos += 1;
        const next = parseAnd();
        if (!next) return null;
        items.push(next);
      }
      return items.length === 1 ? items[0] : { t: "or", items };
    }

    const node = parseOr();
    // Trailing tokens mean the parser stopped early: not a subset it models.
    if (!node || pos !== tokens.length) return null;
    return node;
  }

  /// An operand, or `null` when the node is composite and cannot be one.
  ///
  /// A call is deliberately excluded: `above(delta, threshold(5))` nests one
  /// call inside another, and there is no honest control for that. Such a
  /// condition becomes raw text instead, which is lossless, rather than a
  /// dropdown that quietly drops the inner call.
  function operandFromAst(node) {
    if (!node) return null;
    if (node.t === "lit") return { kind: node.kind, value: node.value };
    if (node.t === "field") return { kind: "field", value: node.name };
    return null;
  }

  function clauseFromAst(node) {
    if (!node) return null;
    if (node.t === "not") {
      const inner = clauseFromAst(node.e);
      if (!inner) return null;
      return Object.assign({}, inner, { not: !inner.not });
    }
    if (node.t === "cmp") {
      const left = operandFromAst(node.left);
      if (!left) return null;
      let tunable = false;
      let rightNode = node.right;
      // `threshold(x)` round-trips as "tunable", not as a call the user made.
      if (rightNode.t === "call" && rightNode.name === "threshold" &&
          rightNode.args.length === 1) {
        tunable = true;
        rightNode = rightNode.args[0];
      }
      const right = operandFromAst(rightNode);
      if (!right) return null;
      return { form: "compare", not: false, left, op: node.op, right, tunable };
    }
    if (node.t === "field") {
      // A bare boolean field is a whole condition: `absorption.detected`.
      return {
        form: "compare",
        not: false,
        left: { kind: "field", value: node.name },
        op: "",
        right: { kind: "number", value: "0" },
        tunable: false,
      };
    }
    if (node.t === "call") {
      const args = node.args.map(operandFromAst);
      if (args.some((a) => a === null)) return null;
      return { form: "call", not: false, func: node.name, args };
    }
    // Anything else -- a bare literal, say -- is not a condition the builder
    // can show.
    // A bare literal is not a condition the builder can show.
    return null;
  }

  /// A row's clauses plus the connective between them, or `null`.
  function clauseListFromAst(node) {
    if (node.t === "and" || node.t === "or") {
      const clauses = node.items.map(clauseFromAst);
      if (clauses.some((c) => c === null)) return null;
      return { joiner: node.t, clauses };
    }
    const one = clauseFromAst(node);
    if (!one) return null;
    return { joiner: "and", clauses: [one] };
  }

  /// One `Conditional` from a document, as a row.
  ///
  /// A condition the parser does not model becomes a single raw clause holding
  /// the text unchanged, so nothing is lost by opening it in the builder.
  function rowFromConditional(conditional) {
    const text = String((conditional && conditional.condition) || "");
    const ast = parseCondition(text);
    const built = ast ? clauseListFromAst(ast) : null;
    const row = {
      timeframe: String((conditional && conditional.timeframe) || ""),
      label: String((conditional && conditional.label) || ""),
      joiner: "and",
      clauses: [],
    };
    if (built) {
      row.joiner = built.joiner;
      row.clauses = built.clauses;
    } else {
      row.clauses = [{ form: "raw", not: false, text }];
    }
    return row;
  }

  // -------------------------------------------------------------------------
  // The form
  // -------------------------------------------------------------------------

  function newClause() {
    return {
      form: "compare",
      not: false,
      left: { kind: "field", value: "close" },
      op: ">",
      right: { kind: "number", value: "0" },
      tunable: false,
    };
  }

  function newRow(timeframe) {
    return { timeframe: timeframe || "", label: "", joiner: "and", clauses: [newClause()] };
  }

  // -------------------------------------------------------------------------
  // Concepts: a measurement a client defines, declared as data.
  //
  // The vocabulary comes from `GET /strategies/schema` -> `concepts:`, same as
  // the rest of the form: the selector names, the comparison names, the window
  // bounds and the per-document cap all come from the Rust enums that enforce
  // them. A second hand-written list would drift and then offer a concept the
  // validator rejects -- which is exactly the shape `docs/06` exists to prevent.
  //
  // The one thing the builder does here that the server does not is *parse a
  // concept's selectors into a form the user can fill*, because
  // `lower: {high: 0}` is not something a dropdown can represent. Anything it
  // cannot model -- a selector the vocabulary does not name, a comparison the
  // vocabulary does not have -- comes back as raw text, same as an unknown
  // condition, so a vocabulary change the builder has not caught up with shows
  // as text rather than silently editing the definition.
  // -------------------------------------------------------------------------

  /// One edge of a concept's band, as a form field.
  ///
  /// The builder does not have a document's concept vocabulary, and neither does
  //  the shell UI -- what lives here is the *form* representation, which is the
  //  same shape as `{{high: 0}}`: a selector name plus a candle index. A future
  //  vocabulary that adds a labeled selector or a free-form edge would land here
  //  as a raw field the builder keeps as text rather than silently editing.
  function selectorFromAst(node) {
    if (!node || node.t !== "call" || node.name !== "selector") return null;
    if (node.args.length !== 1) return null;
    const arg = node.args[0];
    if (!arg || arg.t !== "lit" || (arg.kind !== "number" && arg.kind !== "string")) return null;
    return { selector: String(arg.value), index: arg.kind === "number" ? String(arg.value) : "0" };
  }

  /// A raw selector the builder cannot model, kept as text.
  function rawSelector(text) {
    return { raw: String(text || "").trim() };
  }

  /// Parse one selector from its written form so a concept form can hold it as
  /// data rather than as prose.
  function parseSelector(text) {
    const trimmed = String(text || "").trim();
    if (!trimmed) return rawSelector("");
    // Try the grammar the document is authored in: {{kind: index}}.
    if (trimmed.startsWith("{") && trimmed.endsWith("}")) {
      const inner = trimmed.slice(1, -1);
      const colon = inner.indexOf(":");
      if (colon >= 0) {
        const kind = inner.slice(0, colon).trim();
        const rest = inner.slice(colon + 1).trim();
        if (kind && /^high|low|open|close|mid|volume$/.test(kind) && /^0*[0-9]+$/.test(rest)) {
          return { selector: kind, index: String(Number(rest)) };
        }
      }
    }
    // Anything else -- a bare name, a selector with a label, a future shape --
    // is kept as raw text. The validator is the word on it; the builder only
    // refuses to pretend it understood it.
    return rawSelector(trimmed);
  }

  /// One selector as it reads back out of the form.
  ///
  /// The document model stores a selector as `{selector, index}` in the form,
  /// and writes it as the text `{high: 0}` that the document grammar expects.
  /// This is the text form, used both for the form's own rendering and for the
  /// YAML scalar the server receives.
  function selectorText(sel) {
    if (!sel) return "";
    if (sel.raw !== undefined) return String(sel.raw);
    if (sel.selector && sel.index !== undefined && sel.index !== null) {
      return `{${sel.selector}: ${sel.index}}`;
    }
    // YAML map form produced by `conceptToValue`: {high: 0} -- one key,
    // one non-negative integer value. Restated as the text form the grammar
    // and the parser both expect, so a concept that rode through the emitter
    // back into the form is not silently reshaped.
    if (sel && typeof sel === "object") {
      const keys = Object.keys(sel);
      if (keys.length === 1) {
        const name = keys[0];
        const val = sel[name];
        if (name && /^(open|high|low|close|mid|volume)$/.test(name) &&
            typeof val === "number" && Number.isInteger(val) && val >= 0) {
          return `{${name}: ${val}}`;
        }
      }
    }
    return "";
  }

  /// The canonical selector name for a form field, or null when the builder
  /// cannot represent it.
  function selectorName(sel) {
    if (!sel) return null;
    if (sel.raw !== undefined) return null;
    const name = sel.selector && String(sel.selector).trim();
    if (!name) return null;
    if (!/^open|high|low|close|mid|volume$/.test(name)) return null;
    return name;
  }

  /// The parts of the concept language, as the schema serves them.
  function conceptPartsFromSchema(schema) {
    const parts = (schema && schema.concepts && schema.concepts.parts) || [];
    return parts.map((p) => ({
      name: String(p.name),
      type: String(p.kind),
      reads: String(p.reads),
    }));
  }

  /// The selectors a band edge may be measured from.
  function conceptSelectorsFromSchema(schema) {
    return (schema && schema.concepts && schema.concepts.selectors) || ["high", "low", "open", "close", "mid", "volume"];
  }

  /// The comparisons a `require` entry may use.
  function conceptOpsFromSchema(schema) {
    return (schema && schema.concepts && schema.concepts.ops) || ["below", "above", "below_or_equal", "above_or_equal"];
  }

  /// The window bounds a concept may span.
  function conceptWindowFromSchema(schema) {
    const w = (schema && schema.concepts && schema.concepts.window) || [2, 8];
    return { min: Number(w[0]) || 2, max: Number(w[1]) || 8 };
  }

  /// The maximum concepts one document may declare.
  function conceptMaxFromSchema(schema) {
    return Number((schema && schema.concepts && schema.concepts.max_concepts)) || 5;
  }

  /// The sides a concept may expect a reaction from.
  function conceptSidesFromSchema(schema) {
    return (schema && schema.concepts && schema.concepts.sides) || ["buy", "sell"];
  }

  /// One concept as it appears in a form.
  function newConcept(schema) {
    const sides = conceptSidesFromSchema(schema);
    return {
      name: "",
      label: "",
      side: sides[0] || "buy",
      window: "",
      lower: { selector: conceptSelectorsFromSchema(schema)[1] || "high", index: "0" },
      upper: { selector: conceptSelectorsFromSchema(schema)[2] || "low", index: "2" },
      require: [],
      min_band_ratio: "",
    };
  }

  /// A single requirement line, as a form row.
  function newRequirement(schema) {
    const sel = conceptSelectorsFromSchema(schema);
    const op = conceptOpsFromSchema(schema);
    return { left: { selector: sel[1] || "high", index: "0" }, op: op[0] || "below", right: { selector: sel[2] || "low", index: "2" } };
  }

  /// A concept definition the builder cannot model, kept as raw text.
  function rawConcept(text) {
    return { raw: String(text || "").trim() };
  }

  /// A concept as it reads back out of the form, ready for `documentFromForm`.
  function conceptValue(c) {
    if (!c) return null;
    if (c.raw !== undefined) return { raw: c.raw };
    const name = String(c.name || "").trim();
    if (!name) return null;
    const window = Number(String(c.window || "").trim());
    const lowerSel = c.lower ? parseSelector(selectorText(c.lower)) : null;
    const upperSel = c.upper ? parseSelector(selectorText(c.upper)) : null;
    const require = (c.require || [])
      .map((r) => ({
        left: parseSelector(selectorText(r.left)),
        op: String(r.op || "").trim(),
        right: parseSelector(selectorText(r.right)),
      }))
      .filter((r) => r.left && r.right && r.op);
    const out = {
      name,
      side: String(c.side || "").trim(),
      window,
    };
    if (c.label !== undefined && String(c.label || "").trim()) out.label = String(c.label).trim();
    if (lowerSel && lowerSel.selector && lowerSel.index !== undefined) out.lower = lowerSel;
    if (upperSel && upperSel.selector && upperSel.index !== undefined) out.upper = upperSel;
    if (require.length) out.require = require;
    if (c.min_band_ratio !== undefined && c.min_band_ratio !== "" && /^[-+]?[0-9]*\.?[0-9]+([eE][-+]?[0-9]+)?$/.test(String(c.min_band_ratio).trim())) {
      const v = Number(String(c.min_band_ratio).trim());
      if (v > 0 && isFinite(v)) out.min_band_ratio = v;
    }
    return Object.keys(out).length ? out : null;
  }

  /// The form-level concept model.
  function conceptModelFromSchema(schema) {
    const max = conceptMaxFromSchema(schema);
    return {
      items: Array.from({ length: Math.min(max, 1) }, () => newConcept(schema)),
      max,
    };
  }

  /// Whether a selector reads a price or a volume, for the local concept checks.
  function selectorKind(sel) {
    if (!sel || !sel.selector) return null;
    const n = String(sel.selector).trim();
    if (!n) return null;
    return /^(open|high|low|close|mid)$/.test(n) ? "price" : /^(volume)$/.test(n) ? "volume" : null;
  }

  /// Why a single concept is not yet writable.
  function conceptIssues(c, schema) {
    const items = [];
    if (!c) return items;
    if (c.raw !== undefined) {
      if (!c.raw) items.push("a concept is empty");
      return items;
    }
    const name = String(c.name || "").trim();
    if (!name) items.push("a concept needs a name");
    else if (!/^[a-z][a-z0-9_]*$/.test(name)) items.push("a concept name must be lowercase letters, digits and underscores, and start with a letter");
    else if (name.length > 48) items.push("a concept name must be at most 48 characters");
    if (c.label !== undefined && String(c.label || "").trim().length > 48) items.push("a concept label must be at most 48 characters");
    if (!c.side || !conceptSidesFromSchema(schema).includes(c.side)) items.push("a concept needs a side: " + conceptSidesFromSchema(schema).join(" or "));
    const window = parseInt(String(c.window || "").trim(), 10);
    if (!window || window < 2 || window > 8) items.push("a concept window must be 2..8 candles");
    if (!c.lower || !c.lower.selector || !c.lower.index) items.push("a concept's cheaper edge is not a valid selector, e.g. {high: 0}");
    else if (parseInt(c.lower.index, 10) >= (window || 9)) items.push("the cheaper edge's candle index is outside the window");
    if (!c.upper || !c.upper.selector || !c.upper.index) items.push("a concept's dearer edge is not a valid selector, e.g. {low: 2}");
    else if (parseInt(c.upper.index, 10) >= (window || 9)) items.push("the dearer edge's candle index is outside the window");
    if (c.lower && c.upper && !c.lower.raw && !c.upper.raw && c.lower.selector === c.upper.selector && c.lower.index === c.upper.index) {
      items.push("both band edges are the same selector, so the band has no height");
    }
    if (c.require) {
      for (let i = 0; i < c.require.length; i++) {
        const r = c.require[i];
        if (!r.left || !r.right || !r.op) items.push(`requirement ${i + 1} is incomplete`);
        else if (selectorKind(r.left) !== selectorKind(r.right)) items.push(`requirement ${i + 1} compares different kinds of thing`);
      }
    }
    return items;
  }

  /// Every concept-level issue in a form.
  function conceptFormIssues(model, schema) {
    const issues = [];
    if (!model) return issues;
    const declared = [];
    for (let i = 0; i < model.items.length; i++) {
      const c = model.items[i];
      const its = conceptIssues(c, schema);
      for (const msg of its) issues.push(`concept ${i + 1}: ${msg}`);
      const v = conceptValue(c);
      if (v && v.name) declared.push(v.name);
    }
    for (let i = 0; i < declared.length; i++) {
      for (let j = i + 1; j < declared.length; j++) {
        if (declared[i] === declared[j]) {
          issues.push(`concept ${j + 1}: the name is used twice`);
        }
      }
    }
    return issues;
  }

  /// The full form-level issue list, including concepts.
  function allFormIssues(form, schema) {
    const issues = formIssues(form, schema);
    if (form.concepts) {
      for (const msg of conceptFormIssues(form.concepts, schema)) {
        issues.push(msg);
      }
    }
    return issues;
  }

  /// A form with nothing in it. `schema` supplies the first stop rule and
  /// timeframe, so the default is never something the server would reject.
  function emptyForm(schema) {
    const stops = (schema && schema.stops) || [{ kind: "below_sweep_low", params: [] }];
    const frames = (schema && schema.timeframes) || ["5m"];
    const types = (schema && schema.take_profit_types) || ["risk_multiple"];
    return {
      name: "My strategy",
      version: "1.0",
      kind: "strategy",
      market: "BTCUSDT",
      timeframes: [{ name: "entry", tf: frames.indexOf("5m") >= 0 ? "5m" : frames[0] }],
      direction: "",
      risk: {
        maxRiskPct: "1.0",
        stop: { kind: stops[0].kind, params: {} },
        hasTakeProfit: true,
        takeProfit: { type: types[0], value: "2.0" },
      },
      groups: { entryAll: [], entryAny: [], invalidation: [], exitAll: [], exitAny: [] },
      skillRef: "",
      description: "",
      // No default concept: an empty one would be a permanent "needs a name"
      // issue on every form, and a concept the user never asked for would be
      // emitted into documents they did not write one into. The concept editor
      // creates the model explicitly (`conceptModelFromSchema`), and a document
      // that already carries concepts gets one in `formFromDocument`.
    };
  }

  /// The parameters one stop rule takes.
  function stopParams(schema, kind) {
    const stops = (schema && schema.stops) || [];
    const found = stops.filter((s) => s.kind === kind)[0];
    return found ? found.params : [];
  }

  function stopFromValue(value, schema) {
    const kind = typeof value === "string" ? value : String((value && value.kind) || "");
    const params = {};
    for (const name of stopParams(schema, kind)) {
      const raw = value && typeof value === "object" ? value[name] : undefined;
      params[name] = raw === undefined || raw === null ? "" : String(raw);
    }
    return { kind, params };
  }

  function stopToValue(stop, schema) {
    const names = stopParams(schema, stop.kind);
    if (!names.length) return stop.kind;
    const value = { kind: stop.kind };
    for (const name of names) value[name] = Number(stop.params[name]);
    return value;
  }

  /// Rows to the `Conditional` list a document carries.
  function conditionalsFromRows(rows) {
    const out = [];
    for (const row of rows || []) {
      const condition = rowText(row);
      if (!condition) continue;
      const conditional = { timeframe: row.timeframe, condition };
      if (row.label) conditional.label = row.label;
      out.push(conditional);
    }
    return out;
  }

  /// A form as a `StrategyDocument`, ready to serialize.
  function documentFromForm(form, schema) {
    const doc = {
      name: form.name,
      version: form.version,
      kind: form.kind,
      market: form.market,
      timeframes: {},
    };
    for (const frame of form.timeframes) {
      if (frame.name) doc.timeframes[frame.name] = frame.tf;
    }

    // An indicator has no trade logic; the validator refuses the blocks.
    if (form.kind !== "indicator") {
      const entry = {};
      if (form.direction) entry.direction = form.direction;
      const all = conditionalsFromRows(form.groups.entryAll);
      const any = conditionalsFromRows(form.groups.entryAny);
      if (all.length) entry.all_of = all;
      if (any.length) entry.any_of = any;
      if (Object.keys(entry).length) doc.entry = entry;

      const risk = {
        max_risk_pct: Number(form.risk.maxRiskPct),
        stop: stopToValue(form.risk.stop, schema),
      };
      if (form.risk.hasTakeProfit) {
        risk.take_profit = {
          type: form.risk.takeProfit.type,
          value: Number(form.risk.takeProfit.value),
        };
      }
      doc.risk = risk;

      const invalidation = conditionalsFromRows(form.groups.invalidation);
      if (invalidation.length) doc.invalidation = invalidation;

      const exit = {};
      const exitAll = conditionalsFromRows(form.groups.exitAll);
      const exitAny = conditionalsFromRows(form.groups.exitAny);
      if (exitAll.length) exit.all_of = exitAll;
      if (exitAny.length) exit.any_of = exitAny;
      if (Object.keys(exit).length) doc.exit = exit;
    }

    // The builder made this document, and docs/15 wants the provenance to be
    // truthful: it is one of the three origins a strategy can come from.
    const metadata = { created_by: "visual_builder" };
    if (form.skillRef) metadata.skill_ref = form.skillRef;
    if (form.description) metadata.description = form.description;
    doc.metadata = metadata;

    return doc;
  }

  /// A parsed document as a form.
  ///
  /// `schema` is only used to decide which stop parameters exist.
  function formFromDocument(doc, schema) {
    const form = emptyForm(schema);
    form.name = doc.name == null ? "" : String(doc.name);
    form.version = doc.version == null ? "" : String(doc.version);
    form.kind = doc.kind || "strategy";
    form.market = doc.market || "";

    const frames = Object.keys(doc.timeframes || {}).map((name) => ({
      name,
      tf: String(doc.timeframes[name]),
    }));
    if (frames.length) form.timeframes = frames;

    const entry = doc.entry || {};
    form.direction = entry.direction || "";
    form.groups.entryAll = (entry.all_of || []).map(rowFromConditional);
    form.groups.entryAny = (entry.any_of || []).map(rowFromConditional);
    form.groups.invalidation = (doc.invalidation || []).map(rowFromConditional);
    const exit = doc.exit || {};
    form.groups.exitAll = (exit.all_of || []).map(rowFromConditional);
    form.groups.exitAny = (exit.any_of || []).map(rowFromConditional);

    if (doc.risk) {
      form.risk.maxRiskPct =
        doc.risk.max_risk_pct == null ? "" : String(doc.risk.max_risk_pct);
      form.risk.stop = stopFromValue(doc.risk.stop, schema);
      form.risk.hasTakeProfit = Boolean(doc.risk.take_profit);
      if (doc.risk.take_profit) {
        form.risk.takeProfit = {
          type: doc.risk.take_profit.type,
          value: String(doc.risk.take_profit.value),
        };
      }
    }

    if (doc.concepts && Array.isArray(doc.concepts)) {
      if (!form.concepts) form.concepts = conceptModelFromSchema(schema);
      form.concepts.items = doc.concepts.map((c) => {
        if (typeof c === "string") return rawConcept(c);
        if (c.raw !== undefined) return rawConcept(c.raw);
        const model = newConcept(schema);
        model.name = c.name == null ? "" : String(c.name);
        model.label = c.label == null ? "" : String(c.label);
        model.side = c.side == null ? "" : String(c.side);
        model.window = c.window == null ? "" : String(c.window);
        if (c.lower) model.lower = parseSelector(selectorText(c.lower));
        if (c.upper) model.upper = parseSelector(selectorText(c.upper));
        if (c.require && Array.isArray(c.require)) {
          model.require = c.require.map((r) => ({
            left: parseSelector(selectorText(r.left)),
            op: r.op == null ? "" : String(r.op),
            right: parseSelector(selectorText(r.right)),
          }));
        }
        if (c.min_band_ratio !== undefined && c.min_band_ratio !== null) model.min_band_ratio = String(c.min_band_ratio);
        return model;
      });
    }

    const metadata = doc.metadata || {};
    form.skillRef = metadata.skill_ref == null ? "" : String(metadata.skill_ref);
    form.description = metadata.description == null ? "" : String(metadata.description);
    return form;
  }

  /// How many rows are held as raw text because the parser could not model
  /// them. The shell says so out loud rather than pretending the builder
  /// understood the whole document.
  function rawRowCount(form) {
    let count = 0;
    for (const key of GROUP_KEYS) {
      for (const row of form.groups[key] || []) {
        if ((row.clauses || []).some((c) => c.form === "raw")) count += 1;
      }
    }
    return count;
  }

  /// Why this form cannot be written as a document yet.
  ///
  /// The validator in Rust is the final word and the shell still calls it; this
  /// exists so a half-filled field says what is wrong before a round trip.
  function formIssues(form, schema) {
    const issues = [];
    if (!form.name.trim()) issues.push("name is empty");
    if (!form.version.trim()) issues.push("version is empty");
    if (!form.market.trim()) issues.push("market is empty");

    const frames = form.timeframes.filter((t) => t.name.trim());
    if (!frames.length) issues.push("at least one timeframe is required");
    const seen = {};
    for (const frame of frames) {
      if (seen[frame.name]) issues.push(`timeframe name "${frame.name}" is used twice`);
      seen[frame.name] = true;
    }
    const names = frames.map((t) => t.name);

    if (form.kind !== "indicator") {
      if (!NUMBER.test(String(form.risk.maxRiskPct).trim())) {
        issues.push("risk per trade is not a number");
      }
      for (const name of stopParams(schema, form.risk.stop.kind)) {
        if (!NUMBER.test(String(form.risk.stop.params[name] || "").trim())) {
          issues.push(`stop parameter "${name}" is not a number`);
        }
      }
      if (form.risk.hasTakeProfit && !NUMBER.test(String(form.risk.takeProfit.value).trim())) {
        issues.push("take-profit value is not a number");
      }
      if (!conditionalsFromRows(form.groups.entryAll).length &&
          !conditionalsFromRows(form.groups.entryAny).length) {
        issues.push("no entry condition: the strategy would never trade");
      }
      if (!conditionalsFromRows(form.groups.invalidation).length) {
        issues.push("no invalidation: a tradable document needs at least one");
      }
    }

    for (const key of GROUP_KEYS) {
      for (const row of form.groups[key] || []) {
        if (row.timeframe && names.length && names.indexOf(row.timeframe) < 0) {
          issues.push(`a condition names timeframe "${row.timeframe}", which is not declared`);
        }
        for (const clause of row.clauses || []) {
          if (clause.form === "raw" && !String(clause.text || "").trim()) {
            issues.push(`an empty condition under ${GROUPS[key].path}`);
          }
          if (clause.form === "compare" && clause.op && clause.right &&
              clause.right.kind === "number" &&
              !NUMBER.test(String(clause.right.value).trim())) {
            issues.push(`"${String(clause.right.value)}" is not a number`);
          }
        }
      }
    }
    return issues;
  }

  /// One concept as its YAML-ready value.
  ///
  /// The selector shape `{high: 0}` in a YAML document is written by the
  /// `emitValue` path as a nested mapping `high: 0`, because `emitValue`
  /// recurses into object values. The form's internal `{selector, index}`
  /// representation is restated here as the object the emitter expects, so the
  /// YAML the server receives is exactly the grammar's shape.
  function conceptToValue(c) {
    if (!c) return null;
    if (c.raw !== undefined) return c.raw ? String(c.raw) : null;
    const v = conceptValue(c);
    if (!v) return null;
    // Selectors become the mapping form the emitter recurses into.
    if (v.lower && v.lower.selector && v.lower.index !== undefined) {
      const k = String(v.lower.selector);
      const n = Number(v.lower.index);
      if (Number.isFinite(n) && n >= 0) v.lower = { [k]: n };
    }
    if (v.upper && v.upper.selector && v.upper.index !== undefined) {
      const k = String(v.upper.selector);
      const n = Number(v.upper.index);
      if (Number.isFinite(n) && n >= 0) v.upper = { [k]: n };
    }
    if (v.require) {
      v.require = v.require.map((r) => ({
        left: r.left ? selectorAsMap(r.left) : null,
        op: r.op,
        right: r.right ? selectorAsMap(r.right) : null,
      })).filter((r) => r.left && r.right && r.op);
      if (!v.require.length) delete v.require;
    }
    return v;
  }

  /// A selector as the mapping form `{high: 0}` the emitter recurses into.
  function selectorAsMap(sel) {
    if (!sel) return null;
    if (sel.raw !== undefined) return { raw: sel.raw };
    const name = sel.selector && String(sel.selector).trim();
    const idx = sel.index !== undefined && sel.index !== null ? String(sel.index).trim() : null;
    if (!name || !idx) return null;
    const n = Number(idx);
    if (!Number.isFinite(n) || n < 0) return null;
    return { [name]: n };
  }

  /// A document as YAML text.
  ///
  /// Empty blocks are omitted rather than written as `[]`, which is what the
  /// schema's own serializer does -- the document that comes back from the
  /// server is byte-comparable with the one written here.
  function toYaml(doc) {
    const out = [];
    for (const [key, value] of Object.entries(doc)) {
      if (value === undefined || value === null) continue;
      if (Array.isArray(value) && !value.length) continue;
      if (value && typeof value === "object" && !Array.isArray(value) &&
          !Object.keys(value).length) {
        continue;
      }
      emitValue(key, value, 0, out);
    }
    return `${out.join("\n")}\n`;
  }
  /// A scalar as YAML reads it back.
  ///
  /// Quoted unless it is unambiguously plain: a value that could be read as a
  /// number or a boolean must not be written bare, or `version: 1.0` comes back
  /// as a float and the validator sees the wrong type.
  function scalar(value) {
    if (typeof value === "boolean") return value ? "true" : "false";
    if (typeof value === "number") {
      if (!Number.isFinite(value)) throw new Error(`cannot write ${value} as a number`);
      return String(value);
    }
    const text = String(value);
    if (text === "") return '""';
    if (/^[A-Za-z_][A-Za-z0-9_.-]*$/.test(text) &&
        !/^(true|false|null|yes|no|on|off|~)$/i.test(text)) {
      return text;
    }
    return `"${escapeDouble(text)}"`;
  }

  /// A condition as YAML reads it back.
  ///
  /// Left bare when it can be, which is nearly always: `close > vwap` is a
  /// plain scalar, and so is `market_structure.trend == "bullish"`. Only a
  /// value containing ": " or " #" needs quoting, because those start a mapping
  /// and a comment respectively.
  function conditionScalar(text) {
    if (text === "") return '""';
    const plain =
      /^[A-Za-z0-9_]/.test(text) &&
      text.trim() === text &&
      text.indexOf(": ") < 0 &&
      text.indexOf(" #") < 0 &&
      !/[\n\r]/.test(text) &&
      !text.endsWith(":");
    if (plain) return text;
    return `'${text.replace(/'/g, "''")}'`;
  }

  function scalarFor(key, value) {
    return key === "condition" ? conditionScalar(String(value)) : scalar(value);
  }

  /// Emit the children of a value, one level deeper than `keyCol` (the column
  /// where the owning key was written). The caller has already written that key.
  ///
  /// Used for both the mapping branch of `emitValue` and for nested object/array
  /// values that appear inside a list item, which the old emitter refused.
  function emitValueChildren(value, keyCol, out) {
    const pad = " ".repeat(keyCol + 2);

    if (Array.isArray(value)) {
      for (const item of value) {
        if (item && typeof item === "object") {
          let first = true;
          for (const [k, v] of Object.entries(item)) {
            if (v === undefined || v === null) continue;
            const prefix = first ? `${pad}- ` : `${pad}  `;
            if (v && typeof v === "object") {
              out.push(`${prefix}${k}:`);
              emitValueChildren(v, keyCol + 4, out);
            } else {
              out.push(`${prefix}${k}: ${scalarFor(k, v)}`);
            }
            first = false;
          }
          if (first) out.push(`${pad}- {}`);
        } else {
          out.push(`${pad}- ${scalar(item)}`);
        }
      }
      return;
    }

    if (value && typeof value === "object") {
      if (!Object.keys(value).length) return;
      for (const [k, v] of Object.entries(value)) {
        if (v === undefined || v === null) continue;
        if (v && typeof v === "object") {
          out.push(`${pad}${k}:`);
          emitValueChildren(v, keyCol + 2, out);
        } else {
          out.push(`${pad}${k}: ${scalarFor(k, v)}`);
        }
      }
      return;
    }

    out.push(`${pad}${scalarFor("", value)}`);
  }

  function emitValue(key, value, indent, out) {
    const pad = " ".repeat(indent);

    if (Array.isArray(value)) {
      if (!value.length) return;
      out.push(`${pad}${key}:`);
      emitValueChildren(value, indent, out);
      return;
    }

    if (value && typeof value === "object") {
      if (!Object.keys(value).length) return;
      out.push(`${pad}${key}:`);
      emitValueChildren(value, indent, out);
      return;
    }

    out.push(`${pad}${key}: ${scalarFor(key, value)}`);
  }

  /// A document as YAML text.
  ///
  /// Empty blocks are omitted rather than written as `[]`, which is what the
  /// schema's own `skip_serializing_if` does -- the document that comes back
  /// from the server is byte-comparable with the one written here.
  function toYaml(doc) {
    const out = [];
    for (const [key, value] of Object.entries(doc)) {
      if (value === undefined || value === null) continue;
      if (Array.isArray(value) && !value.length) continue;
      if (value && typeof value === "object" && !Array.isArray(value) &&
          !Object.keys(value).length) {
        continue;
      }
      emitValue(key, value, 0, out);
    }
    return `${out.join("\n")}\n`;
  }

  /// A document with its concepts block attached, ready to serialize.
  ///
  /// This is the parallel to `documentFromForm` for the concept editor: the
  /// rest of the document is identical, and the only change is that the
  /// `concepts:` block is written from the form's concept model rather than
  /// being absent. A concept the builder could not model is kept as raw text
  /// (`{raw: "..."}`), which the validator either accepts or rejects -- the
  /// builder does not pretend it understood a selector the vocabulary does not
  /// name.
  function documentFromConceptForm(form, schema) {
    const doc = documentFromForm(form, schema);
    if (!form.concepts) return doc;
    const items = [];
    for (const c of form.concepts.items || []) {
      const v = conceptToValue(c);
      if (v) items.push(v);
    }
    if (items.length) {
      doc.concepts = items;
    }
    return doc;
  }

  /// The issues a document would have, including concepts -- the word the
  /// validator would give, minus the parts only the server knows.
  ///
  /// The server is still the final word (`docs/14`: the builder sandboxes, it
  /// does not validate). This exists so a half-filled concept says what is wrong
  /// before a round trip, the same as the rest of `formIssues`.
  function documentIssues(form, schema) {
    const issues = allFormIssues(form, schema);
    if (form.concepts && form.concepts.items) {
      for (const c of form.concepts.items) {
        const v = conceptValue(c);
        if (!v) continue;
        if (v.raw !== undefined) continue;
        try {
          const co = analytics_core_concept_validate(v);
          if (!co.ok) issues.push(`concept \`${v.name}\`: ${co.error}`);
        } catch (e) {
          // The builder's own model disagrees with the grammar -- keep the
          // pre-validation issues above and skip the server's word here.
        }
      }
    }
    return issues;
  }

  // A tiny shim so `documentIssues` can call the validator without pulling the
  // analytics crate into the browser bundle. The shell replays one concept
  // through `POST /strategies/validate` before accepting any definition, so this
  // is only a local sanity check; the real word is the server's. Keep the shim
  // column-stable with `analytics_core::concepts::validate` so a fix here is a
  // hint, not a second opinion.
  function analytics_core_concept_validate(concept) {
    const name = concept.name && String(concept.name);
    if (!name) return { ok: false, error: "a name is required" };
    if (!/^[a-z][a-z0-9_]*$/.test(name)) return { ok: false, error: "a name must be lowercase letters, digits and underscores, starting with a letter" };
    if (name.length > 48) return { ok: false, error: "a name must be at most 48 characters" };
    const window = Number(concept.window);
    if (!window || window < 2 || window > 8) return { ok: false, error: "window must be 2..8 candles" };
    const sides = ["buy", "sell"];
    if (!concept.side || !sides.includes(concept.side)) return { ok: false, error: "side must be buy or sell" };
    const parseSel = (s) => {
      if (!s) return null;
      if (s.raw !== undefined) return s;
      const kind = s.selector && String(s.selector).trim();
      const raw = s.index !== undefined && s.index !== null ? String(s.index).trim() : "";
      const idx = raw === "" ? NaN : parseInt(raw, 10);
      if (!kind || !isFinite(idx) || idx < 0 || idx >= window) return null;
      return { selector: kind, index: idx };
    };
    const kindOf = (sel) => {
      if (!sel || !sel.selector) return null;
      const n = String(sel.selector).trim();
      if (!n) return null;
      return /^(open|high|low|close|mid)$/.test(n) ? "price" : /^(volume)$/.test(n) ? "volume" : null;
    };
    const lower = parseSel(concept.lower);
    const upper = parseSel(concept.upper);
    if (!lower || !upper) return { ok: false, error: "both band edges must be valid selectors inside the window" };
    if (selectorKind(lower) !== "price") return { ok: false, error: "the band's cheaper edge must be a price selector, not volume" };
    if (selectorKind(upper) !== "price") return { ok: false, error: "the band's dearer edge must be a price selector, not volume" };
    if (lower.selector === upper.selector && lower.index === upper.index) return { ok: false, error: "both band edges are the same selector, so the band has no height" };
    if (concept.require) {
      for (const r of concept.require) {
        const l = parseSel(r.left);
        const rt = parseSel(r.right);
        if (!l || !rt) return { ok: false, error: "every requirement operand must be a valid selector inside the window" };
        if (selectorKind(l) !== selectorKind(rt)) return { ok: false, error: "a requirement compares different kinds of thing" };
      }
    }
    if (concept.min_band_ratio !== undefined && concept.min_band_ratio !== null) {
      const r = Number(concept.min_band_ratio);
      if (!isFinite(r) || r <= 0) return { ok: false, error: "min_band_ratio must be a positive finite number" };
    }
    return { ok: true };
  }

  return {
    GROUPS,
    GROUP_KEYS,
    clauseText,
    conditionalsFromRows,
    documentFromForm,
    emptyForm,
    formFromDocument,
    formIssues,
    newClause,
    newRow,
    parseCondition,
    rawRowCount,
    rowFromConditional,
    rowText,
    stopParams,
    toYaml,
    documentFromConceptForm,
    documentIssues,
    conceptModelFromSchema,
    conceptValue,
    conceptToValue,
    parseSelector,
    selectorText,
    rawConcept,
    newConcept,
    newRequirement,
    conceptPartsFromSchema,
    conceptSelectorsFromSchema,
    conceptOpsFromSchema,
    conceptWindowFromSchema,
    conceptMaxFromSchema,
    conceptSidesFromSchema,
    conceptFormIssues,
    allFormIssues,
    analytics_core_concept_validate,
  };
});
