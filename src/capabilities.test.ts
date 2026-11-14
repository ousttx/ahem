import { describe, expect, it } from "vitest";
import ts from "typescript";
import mainCapability from "../src-tauri/capabilities/default.json";
import buildSource from "../src-tauri/build.rs?raw";
import trayCapability from "../src-tauri/capabilities/tray.json";
import indicatorCapability from "../src-tauri/capabilities/indicator.json";

// The quick panel and the indicator may call only the commands their capabilities allow; a
// command they call without a permission fails at run time, and the button that calls it does
// nothing. The commands a page calls are found in its code and in what it uses of the modules it
// imports, followed function by function: a shared module (the History page's, for one) also holds
// commands of the main window, which a page that takes one hook from it never calls.

const SOURCES = import.meta.glob<string>(["./**/*.ts", "./**/*.tsx", "!./**/*.test.ts"], {
  query: "?raw",
  import: "default",
  eager: true,
});

/// Commands named by a template: every name it can make.
const TEMPLATES: Record<string, string[]> = {
  // The quick panel's meeting button: start, or stop the one recording.
  "`${action}_meeting`": ["start_meeting", "stop_meeting"],
};

/// What a piece of code calls: the backend commands it invokes, the calls whose command it does
/// not name (a runner of commands it is given), and the names it uses.
interface Uses {
  commands: Set<string>;
  unnamed: Set<string>;
  names: Set<string>;
}

interface Module {
  /// The modules it imports for their values (not only their types), which run once it loads.
  loads: string[];
  /// A local name of an imported value → its module and its name there.
  imports: Map<string, { from: string; name: string }>;
  /// What each top-level declaration uses once it is called or read.
  declarations: Map<string, Uses>;
  /// What the module does as it loads.
  onLoad: Uses;
}

function resolve(from: string, specifier: string): string | null {
  if (!specifier.startsWith(".")) return null;
  const parts = from.split("/").slice(0, -1);
  for (const part of specifier.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== ".") parts.push(part);
  }
  const path = parts.join("/");
  return [path, `${path}.ts`, `${path}.tsx`, `${path}/index.ts`].find((candidate) => candidate in SOURCES) ?? null;
}

const isFunction = (node: ts.Node) =>
  ts.isFunctionLike(node) || ts.isArrowFunction(node) || ts.isFunctionExpression(node) || ts.isClassLike(node);

/// Collects what `node` uses; `deep: false` leaves out the bodies of the functions in it, which
/// do not run when it does.
function collect(node: ts.Node, invokes: Set<string>, file: string, uses: Uses, deep = true) {
  const visit = (child: ts.Node) => {
    if (ts.isTypeNode(child) || ts.isInterfaceDeclaration(child) || ts.isTypeAliasDeclaration(child)) return;
    if (!deep && isFunction(child)) return;
    if (ts.isCallExpression(child) && ts.isIdentifier(child.expression) && invokes.has(child.expression.text)) {
      const argument = child.arguments[0];
      const commands = commandsOf(argument);
      if (commands) commands.forEach((command) => uses.commands.add(command));
      else uses.unnamed.add(`${file}: invoke(${argument?.getText() ?? ""})`);
      child.arguments.forEach(visit);
      return;
    }
    if (ts.isIdentifier(child)) {
      if (invokes.has(child.text)) throw new Error(`${file}: \`invoke\` is passed on, not called: list what it calls here`);
      uses.names.add(child.text);
      return;
    }
    // A property's name is not a name in scope.
    if (ts.isPropertyAccessExpression(child)) return visit(child.expression);
    if (ts.isPropertyAssignment(child) || ts.isJsxAttribute(child)) {
      if (ts.isPropertyAssignment(child) && ts.isComputedPropertyName(child.name)) visit(child.name);
      if (child.initializer) visit(child.initializer);
      return;
    }
    ts.forEachChild(child, visit);
  };
  visit(node);
}

/// The commands an `invoke` can call by its first argument; `null` when the code does not say.
function commandsOf(argument: ts.Expression | undefined): string[] | null {
  if (argument && (ts.isStringLiteral(argument) || ts.isNoSubstitutionTemplateLiteral(argument))) return [argument.text];
  return TEMPLATES[argument?.getText() ?? ""] ?? null;
}

const noUses = (): Uses => ({ commands: new Set(), unnamed: new Set(), names: new Set() });

function parse(file: string): Module {
  const source = ts.createSourceFile(file, SOURCES[file], ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  const module: Module = { loads: [], imports: new Map(), declarations: new Map(), onLoad: noUses() };
  const invokes = new Set<string>();
  const statements: ts.Statement[] = [];
  for (const statement of source.statements) {
    if (!ts.isImportDeclaration(statement)) {
      statements.push(statement);
      continue;
    }
    const specifier = (statement.moduleSpecifier as ts.StringLiteral).text;
    const clause = statement.importClause;
    if (clause?.isTypeOnly) continue;
    if (specifier === "@tauri-apps/api/core") {
      for (const element of clause?.namedBindings && ts.isNamedImports(clause.namedBindings) ? clause.namedBindings.elements : []) {
        if ((element.propertyName ?? element.name).text === "invoke") invokes.add(element.name.text);
      }
      continue;
    }
    const from = resolve(file, specifier);
    if (from === null) continue;
    module.loads.push(from);
    if (clause?.name) module.imports.set(clause.name.text, { from, name: "default" });
    const bindings = clause?.namedBindings;
    if (bindings && ts.isNamespaceImport(bindings)) throw new Error(`${file}: a namespace import is not followed`);
    for (const element of bindings && ts.isNamedImports(bindings) ? bindings.elements : []) {
      if (!element.isTypeOnly) module.imports.set(element.name.text, { from, name: (element.propertyName ?? element.name).text });
    }
  }
  const declare = (name: string, node: ts.Node) => {
    const uses = module.declarations.get(name) ?? noUses();
    collect(node, invokes, file, uses);
    module.declarations.set(name, uses);
  };
  for (const statement of statements) {
    if (ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)) continue;
    if (ts.isExportDeclaration(statement)) throw new Error(`${file}: an export list is not followed`);
    if ((ts.isFunctionDeclaration(statement) || ts.isClassDeclaration(statement)) && statement.name) {
      declare(statement.name.text, statement);
      if (statement.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.DefaultKeyword)) declare("default", statement);
    } else if (ts.isVariableStatement(statement)) {
      for (const declaration of statement.declarationList.declarations) {
        if (!declaration.initializer) continue;
        const names: string[] = [];
        const name = (binding: ts.BindingName) => {
          if (ts.isIdentifier(binding)) names.push(binding.text);
          else binding.elements.forEach((element) => !ts.isOmittedExpression(element) && name(element.name));
        };
        name(declaration.name);
        for (const each of names) declare(each, declaration.initializer);
        // The value is made as the module loads; the functions in it run only when called.
        collect(declaration.initializer, invokes, file, module.onLoad, false);
      }
    } else if (ts.isExportAssignment(statement)) {
      declare("default", statement.expression);
    } else {
      collect(statement, invokes, file, module.onLoad);
    }
  }
  return module;
}

const MODULES = new Map<string, Module>();
function moduleOf(file: string): Module {
  let module = MODULES.get(file);
  if (!module) {
    module = parse(file);
    MODULES.set(file, module);
  }
  return module;
}

/// The commands the page `file` can call: its own code, everything its imports run as they load,
/// and the declarations reached from there through the names they use.
function pageCommands(file: string): Set<string> {
  const commands = new Set<string>();
  const seen = new Set<string>();
  const pending: Array<[string, string | null]> = [];
  const take = (from: string, uses: Uses) => {
    // A page that reaches a runner of commands: which ones it runs is up to its callers, so they
    // go into TEMPLATES by hand.
    if (uses.unnamed.size > 0) throw new Error(`${file} calls commands this test cannot name: ${[...uses.unnamed].join(", ")}`);
    for (const command of uses.commands) commands.add(command);
    const module = moduleOf(from);
    for (const name of uses.names) {
      if (module.declarations.has(name)) pending.push([from, name]);
      const imported = module.imports.get(name);
      if (imported) pending.push([imported.from, imported.name]);
    }
  };
  // Every module of the page's import graph runs as it loads.
  const loaded = new Set<string>();
  const load = (from: string) => {
    if (loaded.has(from)) return;
    loaded.add(from);
    pending.push([from, null]);
    moduleOf(from).loads.forEach(load);
  };
  load(file);
  // The page itself is all used.
  for (const name of moduleOf(file).declarations.keys()) pending.push([file, name]);
  while (pending.length > 0) {
    const [from, name] = pending.pop()!;
    const key = `${from}#${name ?? ""}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const module = moduleOf(from);
    const uses = name === null ? module.onLoad : module.declarations.get(name);
    if (uses) take(from, uses);
  }
  return commands;
}

/// The app's commands a capability allows: `allow-get-settings` → `get_settings`.
function allowed(capability: { permissions: string[] }): string[] {
  return capability.permissions
    .filter((permission) => permission.startsWith("allow-"))
    .map((permission) => permission.slice("allow-".length).replace(/-/g, "_"))
    .sort();
}

describe("the capabilities of the windows", () => {
  it("allow the main window every command of the backend", () => {
    const commands = [...buildSource.slice(buildSource.indexOf("COMMANDS"), buildSource.indexOf("];")).matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
    expect(commands.length).toBeGreaterThan(50);
    expect(commands.filter((command) => !allowed(mainCapability).includes(command))).toEqual([]);
  });

  it("allow the quick panel every command it calls, and only those", () => {
    expect([...pageCommands("./tray.tsx")].sort()).toEqual(allowed(trayCapability));
  });

  it("allow the indicator every command it calls, and only those", () => {
    expect([...pageCommands("./indicator.tsx")].sort()).toEqual(allowed(indicatorCapability));
  });

  it("are checked against what a page uses of a shared module, not all the module holds", () => {
    const tray = pageCommands("./tray.tsx");
    // The quick panel reads the history through the History page's hook; the page's own
    // commands stay with the main window.
    expect(tray.has("get_history")).toBe(true);
    expect(tray.has("delete_history_entry")).toBe(false);
    // Each command the meeting button can name.
    expect(tray.has("start_meeting") && tray.has("stop_meeting")).toBe(true);
    // A command the page would call through a shared module is found.
    expect(pageCommands("./indicator.tsx").has("get_settings")).toBe(true);
  });
});
