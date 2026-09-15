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

  // -------------------------------------------------------------------------
  // Writing YAML
  // -------------------------------------------------------------------------

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

  function emitValue(key, value, indent, out) {
    const pad = " ".repeat(indent);

    if (Array.isArray(value)) {
      if (!value.length) return;
      out.push(`${pad}${key}:`);
      for (const item of value) {
        if (item && typeof item === "object") {
          let first = true;
          for (const [k, v] of Object.entries(item)) {
            if (v === undefined || v === null) continue;
            if (v && typeof v === "object") {
              throw new Error(`cannot nest an object inside a list item (${key}.${k})`);
            }
            out.push(`${pad}  ${first ? "- " : "  "}${k}: ${scalarFor(k, v)}`);
            first = false;
          }
          if (first) out.push(`${pad}  - {}`);
        } else {
          out.push(`${pad}  - ${scalar(item)}`);
        }
      }
      return;
    }

    if (value && typeof value === "object") {
      if (!Object.keys(value).length) return;
      out.push(`${pad}${key}:`);
      for (const [k, v] of Object.entries(value)) {
        if (v === undefined || v === null) continue;
        if (v && typeof v === "object") emitValue(k, v, indent + 2, out);
        else out.push(`${pad}  ${k}: ${scalarFor(k, v)}`);
      }
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
  };
});
