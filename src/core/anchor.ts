import { Node, Project, SourceFile, SyntaxKind } from "ts-morph";
import type { AnchorInfo } from "./types.js";

export function extractAnchors(filePath: string, content: string): Record<string, AnchorInfo> {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: {
      allowJs: true,
      checkJs: false
    }
  });
  const sourceFile = project.createSourceFile(filePath, content, { overwrite: true });
  const anchors = new Map<string, AnchorInfo>();
  const counts = new Map<string, number>();

  const add = (baseAnchor: string, node: Node, kind: AnchorInfo["kind"]) => {
    const seen = counts.get(baseAnchor) ?? 0;
    counts.set(baseAnchor, seen + 1);
    const anchor = seen === 0 ? baseAnchor : `${baseAnchor}#${seen + 1}`;
    anchors.set(anchor, {
      anchor,
      start: node.getStartLineNumber(),
      end: node.getEndLineNumber(),
      complexity: cyclomaticComplexity(node),
      kind
    });
  };

  for (const fn of sourceFile.getFunctions()) {
    const name = fn.getName();
    if (name) add(`fn:${name}`, fn, "function");
  }

  for (const cls of sourceFile.getClasses()) {
    const className = cls.getName();
    if (className) add(`class:${className}`, cls, "class");
    for (const method of cls.getMethods()) {
      if (className) add(`fn:${className}.${method.getName()}`, method, "method");
      else add(`fn:${method.getName()}`, method, "method");
    }
  }

  for (const variable of sourceFile.getVariableDeclarations()) {
    const initializer = variable.getInitializer();
    if (initializer && (Node.isArrowFunction(initializer) || Node.isFunctionExpression(initializer))) {
      add(`fn:${variable.getName()}`, initializer, "function");
    }
  }

  for (const exported of sourceFile.getExportedDeclarations().entries()) {
    const [name, declarations] = exported;
    const declaration = declarations[0];
    if (!declaration) continue;
    if (!anchors.has(`export:${name}`)) add(`export:${name}`, declaration, "export");
  }

  addSignificantBlocks(sourceFile, add);

  return Object.fromEntries(anchors);
}

function addSignificantBlocks(
  sourceFile: SourceFile,
  add: (anchor: string, node: Node, kind: AnchorInfo["kind"]) => void
): void {
  for (const node of sourceFile.getDescendantsOfKind(SyntaxKind.IfStatement)) {
    const expression = node.getExpression().getText();
    const normalized = expression
      .replace(/[^a-zA-Z0-9_$.]+/g, "_")
      .replace(/^_+|_+$/g, "")
      .slice(0, 48);
    if (cyclomaticComplexity(node) >= 3 && normalized) {
      add(`block:if_${normalized}`, node, "block");
    }
  }
}

export function anchorExists(filePath: string, content: string, anchor: string): boolean {
  return Object.prototype.hasOwnProperty.call(extractAnchors(filePath, content), anchor);
}

export function cyclomaticComplexity(node: Node): number {
  let complexity = 1;
  node.forEachDescendant((descendant) => {
    switch (descendant.getKind()) {
      case SyntaxKind.IfStatement:
      case SyntaxKind.ForStatement:
      case SyntaxKind.ForInStatement:
      case SyntaxKind.ForOfStatement:
      case SyntaxKind.WhileStatement:
      case SyntaxKind.DoStatement:
      case SyntaxKind.CaseClause:
      case SyntaxKind.CatchClause:
      case SyntaxKind.ConditionalExpression:
        complexity += 1;
        break;
      case SyntaxKind.BinaryExpression: {
        const text = descendant.getText();
        if (text.includes("&&") || text.includes("||") || text.includes("??")) complexity += 1;
        break;
      }
    }
  });
  return complexity;
}
