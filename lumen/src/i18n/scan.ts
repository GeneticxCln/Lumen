import ts from "typescript";

/** A piece of user-visible text written directly in a component instead of coming from the catalog. */
export interface Hit {
  line: number;
  text: string;
}

/** The attributes that carry text for people (and screen readers). */
const TEXT_ATTRIBUTES = new Set(["aria-label", "aria-description", "aria-placeholder", "title", "placeholder", "alt", "label"]);

const hasLetter = (text: string) => /\p{L}/u.test(text);

/**
 * The string pieces of an expression that end up on screen: literals, templates, and the branches
 * of `a ? "x" : "y"`, `a && "x"`, `a ?? "x"` and `"x" + b`. Calls and property reads are not
 * followed, so `t("key")` and `error.message` pass.
 */
function textIn(node: ts.Node, found: ts.Node[]): void {
  if (ts.isParenthesizedExpression(node) || ts.isAsExpression(node) || ts.isNonNullExpression(node) || ts.isSatisfiesExpression(node)) {
    textIn(node.expression, found);
  } else if (ts.isConditionalExpression(node)) {
    textIn(node.whenTrue, found);
    textIn(node.whenFalse, found);
  } else if (ts.isBinaryExpression(node)) {
    const kind = node.operatorToken.kind;
    if (
      kind === ts.SyntaxKind.AmpersandAmpersandToken ||
      kind === ts.SyntaxKind.BarBarToken ||
      kind === ts.SyntaxKind.QuestionQuestionToken ||
      kind === ts.SyntaxKind.PlusToken
    ) {
      textIn(node.left, found);
      textIn(node.right, found);
    }
  } else if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isTemplateExpression(node)) {
    found.push(node);
  }
}

function staticText(node: ts.Node): string {
  if (ts.isTemplateExpression(node)) return node.head.text + node.templateSpans.map((span) => span.literal.text).join("");
  return (node as ts.StringLiteral).text;
}

/** Hard-coded user-visible text in one TSX source: JSX text, text attributes, and strings in JSX expressions. */
export function findHardCodedText(source: string, fileName = "file.tsx"): Hit[] {
  const file = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const hits: Hit[] = [];

  const add = (node: ts.Node, text: string) => {
    if (!hasLetter(text)) return;
    hits.push({ line: file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1, text: text.replace(/\s+/g, " ").trim() });
  };
  const addExpression = (expression: ts.Expression) => {
    const found: ts.Node[] = [];
    textIn(expression, found);
    for (const node of found) add(node, staticText(node));
  };

  const visit = (node: ts.Node) => {
    if (ts.isJsxText(node)) {
      add(node, node.text);
    } else if (ts.isJsxAttribute(node) && TEXT_ATTRIBUTES.has(node.name.getText(file))) {
      const value = node.initializer;
      if (value && ts.isStringLiteral(value)) add(value, value.text);
      else if (value && ts.isJsxExpression(value) && value.expression) addExpression(value.expression);
    } else if (ts.isJsxExpression(node) && (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent)) && node.expression) {
      addExpression(node.expression);
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return hits;
}

/** Every string literal in a source, for finding which catalog keys are referenced. */
export function stringLiterals(source: string, fileName = "file.ts"): Set<string> {
  const kind = fileName.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
  const file = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, kind);
  const found = new Set<string>();
  const visit = (node: ts.Node) => {
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) found.add(node.text);
    ts.forEachChild(node, visit);
  };
  visit(file);
  return found;
}
