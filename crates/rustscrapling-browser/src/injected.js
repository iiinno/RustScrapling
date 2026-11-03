// RustScrapling utility-world helpers. Evaluated only inside an isolated
// world created with Page.createIsolatedWorld, never in the page's own JS
// world, so page scripts cannot observe or tamper with these functions.
//
// The selector and actionability semantics follow the public behaviour of
// Playwright locators that the pinned Scrapling v0.5 MCP server relies on
// (strict single-element targets, CSS/XPath/text/role/aria-ref engines,
// visible/enabled/editable checks, hit-target verification). This is an
// independent implementation, not a copy of Playwright's injected script.
(() => {
  if (globalThis.__rsx) return true;

  const refs = new Map();
  let refGeneration = 0;

  const normalize = (text) => String(text).replace(/[​­]/g, '').replace(/\s+/g, ' ').trim();
  const isElement = (node) => node && node.nodeType === 1;

  function parentElementOrShadowHost(element) {
    if (element.parentElement) return element.parentElement;
    const parent = element.parentNode;
    if (parent && parent.nodeType === 11 && parent.host) return parent.host;
    return null;
  }

  function composedContains(ancestor, node) {
    for (let current = node; current; ) {
      if (current === ancestor) return true;
      if (current.parentNode) current = current.parentNode;
      else if (current.host) current = current.host;
      else return false;
    }
    return false;
  }

  // Elements below `scope` in composed (shadow-including) document order.
  function composedElements(scope) {
    const out = [];
    const visit = (node) => {
      if (node.shadowRoot) visitChildren(node.shadowRoot);
      visitChildren(node);
    };
    const visitChildren = (node) => {
      for (let child = node.firstElementChild; child; child = child.nextElementSibling) {
        out.push(child);
        visit(child);
      }
    };
    if (scope.nodeType === 9) visitChildren(scope);
    else visit(scope);
    return out;
  }

  const SKIP_TEXT = new Set(['SCRIPT', 'NOSCRIPT', 'STYLE', 'TEMPLATE', 'HEAD']);

  function elementText(cache, root) {
    let value = cache.get(root);
    if (value !== undefined) return value;
    value = '';
    if (root.nodeType === 1 && SKIP_TEXT.has(root.nodeName)) {
      cache.set(root, value);
      return value;
    }
    if (root.nodeType === 1 && root.shadowRoot) value += elementText(cache, root.shadowRoot);
    for (let child = root.firstChild; child; child = child.nextSibling) {
      if (child.nodeType === 3) value += child.nodeValue;
      else if (child.nodeType === 1) value += elementText(cache, child);
    }
    cache.set(root, value);
    return value;
  }

  function unquote(text) {
    const quote = text[0];
    let out = '';
    for (let i = 1; i < text.length - 1; i++) {
      if (text[i] === '\\' && i + 1 < text.length - 1) i++;
      out += text[i];
    }
    if (text[text.length - 1] !== quote) throw new Error(`Unterminated string in selector: ${text}`);
    return out;
  }

  // Parse a text matcher body: "exact", 'exact', /regex/flags, or lax text.
  // A trailing i/s after a quoted string selects case-insensitive/sensitive
  // full-string matching (Playwright internal:* syntax).
  function textMatcher(body, laxSubstring) {
    body = body.trim();
    const regex = /^\/(.*)\/([dgimsuvy]*)$/s.exec(body);
    if (regex) {
      const re = new RegExp(regex[1], regex[2].replace('g', ''));
      return { kind: 'regex', match: (text) => re.test(normalize(text)) };
    }
    const quoted = /^(["'])(.*)\1([is]?)$/s.exec(body);
    if (quoted) {
      const value = normalize(unquote(quoted[1] + quoted[2] + quoted[1]));
      if (quoted[3] === 'i') {
        const lower = value.toLowerCase();
        return { kind: 'lax', match: (text) => normalize(text).toLowerCase().includes(lower) };
      }
      return { kind: 'strict', match: (text) => normalize(text) === value };
    }
    const lower = normalize(body).toLowerCase();
    if (laxSubstring === false) return { kind: 'strict', match: (text) => normalize(text).toLowerCase() === lower };
    return { kind: 'lax', match: (text) => normalize(text).toLowerCase().includes(lower) };
  }

  function textQuery(scope, body) {
    const { kind, match } = textMatcher(body);
    const cache = new Map();
    const result = [];
    let lastNone = null;
    for (const element of composedElements(scope)) {
      if (SKIP_TEXT.has(element.nodeName)) continue;
      if (element.ownerDocument.head && element.ownerDocument.head.contains(element)) continue;
      if (kind === 'lax' && lastNone && composedContains(lastNone, element)) continue;
      if (!match(elementText(cache, element))) {
        lastNone = element;
        continue;
      }
      let childMatches = false;
      for (let child = element.firstElementChild; child; child = child.nextElementSibling) {
        if (match(elementText(cache, child))) {
          childMatches = true;
          break;
        }
      }
      if (!childMatches && element.shadowRoot && match(elementText(cache, element.shadowRoot))) childMatches = true;
      if (!childMatches || kind === 'strict') result.push(element);
    }
    return result;
  }

  function cssQuery(scope, selector) {
    try {
      (scope.nodeType === 9 ? scope : scope.ownerDocument).querySelector(selector);
    } catch (error) {
      throw new Error(`Unexpected token in CSS selector "${selector}"`);
    }
    return composedElements(scope).filter((element) => element.matches(selector));
  }

  function xpathQuery(scope, expression) {
    if (scope.nodeType !== 9 && expression.startsWith('/')) expression = '.' + expression;
    const doc = scope.nodeType === 9 ? scope : scope.ownerDocument;
    let snapshot;
    try {
      snapshot = doc.evaluate(expression, scope, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    } catch (error) {
      throw new Error(`Invalid XPath "${expression}": ${error.message}`);
    }
    const out = [];
    for (let i = 0; i < snapshot.snapshotLength; i++) {
      const node = snapshot.snapshotItem(i);
      if (isElement(node)) out.push(node);
    }
    return out;
  }

  // ---------------------------------------------------------------- ARIA
  const IMPLICIT_INPUT_ROLES = {
    button: 'button', image: 'button', reset: 'button', submit: 'button', checkbox: 'checkbox',
    radio: 'radio', range: 'slider', number: 'spinbutton', search: 'searchbox', email: 'textbox',
    tel: 'textbox', text: 'textbox', url: 'textbox', password: 'textbox',
  };
  const HEADINGS = { H1: 1, H2: 2, H3: 3, H4: 4, H5: 5, H6: 6 };

  function hasGlobalAria(element) {
    for (const attribute of element.attributes)
      if (attribute.name.startsWith('aria-') && !['aria-hidden', 'aria-disabled'].includes(attribute.name)) return true;
    return false;
  }

  function implicitRole(element) {
    const tag = element.nodeName;
    switch (tag) {
      case 'A': case 'AREA': return element.hasAttribute('href') ? 'link' : null;
      case 'ARTICLE': return 'article';
      case 'ASIDE': return 'complementary';
      case 'BLOCKQUOTE': return 'blockquote';
      case 'BUTTON': return 'button';
      case 'CAPTION': return 'caption';
      case 'CODE': return 'code';
      case 'DATALIST': return 'listbox';
      case 'DD': return 'definition';
      case 'DEL': return 'deletion';
      case 'DETAILS': return 'group';
      case 'DFN': return 'term';
      case 'DIALOG': return 'dialog';
      case 'DT': return 'term';
      case 'EM': return 'emphasis';
      case 'FIELDSET': return 'group';
      case 'FIGURE': return 'figure';
      case 'FOOTER': return element.closest('article,aside,main,nav,section') ? null : 'contentinfo';
      case 'FORM': return 'form';
      case 'H1': case 'H2': case 'H3': case 'H4': case 'H5': case 'H6': return 'heading';
      case 'HEADER': return element.closest('article,aside,main,nav,section') ? null : 'banner';
      case 'HR': return 'separator';
      case 'HTML': return 'document';
      case 'IMG':
        if (element.getAttribute('alt') === '' && !element.getAttribute('title') && !hasGlobalAria(element)) return 'presentation';
        return 'img';
      case 'INPUT': {
        const type = (element.getAttribute('type') || 'text').toLowerCase();
        if (type === 'hidden') return null;
        if (type === 'file') return 'button';
        if (['text', 'search', 'email', 'tel', 'url', ''].includes(type) && element.hasAttribute('list')) return 'combobox';
        return IMPLICIT_INPUT_ROLES[type] || 'textbox';
      }
      case 'INS': return 'insertion';
      case 'LI': return 'listitem';
      case 'MAIN': return 'main';
      case 'MARK': return 'mark';
      case 'MATH': return 'math';
      case 'MENU': return 'list';
      case 'METER': return 'meter';
      case 'NAV': return 'navigation';
      case 'OL': case 'UL': return 'list';
      case 'OPTGROUP': return 'group';
      case 'OPTION': return 'option';
      case 'OUTPUT': return 'status';
      case 'P': return 'paragraph';
      case 'PROGRESS': return 'progressbar';
      case 'SEARCH': return 'search';
      case 'SECTION': return (element.hasAttribute('aria-label') || element.hasAttribute('aria-labelledby')) ? 'region' : null;
      case 'SELECT': return element.multiple || element.size > 1 ? 'listbox' : 'combobox';
      case 'STRONG': return 'strong';
      case 'SUB': return 'subscript';
      case 'SUP': return 'superscript';
      case 'SVG': case 'svg': return 'img';
      case 'TABLE': return 'table';
      case 'TBODY': case 'THEAD': case 'TFOOT': return 'rowgroup';
      case 'TD': return 'cell';
      case 'TEXTAREA': return 'textbox';
      case 'TH': return element.closest('thead') || element.getAttribute('scope') === 'col' ? 'columnheader' : 'rowheader';
      case 'TIME': return 'time';
      case 'TR': return 'row';
      case 'DIV': case 'SPAN': case 'BODY': return 'generic';
      default: return null;
    }
  }

  function ariaRole(element) {
    const explicit = (element.getAttribute('role') || '').trim().split(/\s+/)[0];
    if (explicit) {
      if ((explicit === 'none' || explicit === 'presentation') && (hasGlobalAria(element) || element.tabIndex >= 0 && element.hasAttribute('tabindex'))) return implicitRole(element);
      return explicit;
    }
    return implicitRole(element);
  }

  function isHiddenForAria(element) {
    for (let current = element; current; current = parentElementOrShadowHost(current)) {
      if (current.getAttribute('aria-hidden') === 'true') return true;
      if (['STYLE', 'SCRIPT', 'NOSCRIPT', 'TEMPLATE'].includes(current.nodeName)) return true;
    }
    const style = getComputedStyle(element);
    if (style.display === 'none') return true;
    if (element.checkVisibility && !element.checkVisibility()) {
      if (style.display !== 'contents') return true;
    }
    return style.visibility !== 'visible' && element.nodeName !== 'OPTION';
  }

  const NAME_FROM_CONTENT = new Set(['button', 'cell', 'checkbox', 'columnheader', 'gridcell', 'heading', 'link',
    'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'radio', 'row', 'rowheader', 'switch', 'tab',
    'tooltip', 'treeitem', 'caption', 'term', 'sectionhead', 'code', 'strong', 'emphasis', 'deletion', 'insertion',
    'subscript', 'superscript', 'mark', 'time', 'definition']);

  function textAlternative(node, visited, fromContent) {
    if (node.nodeType === 3) return node.nodeValue;
    if (!isElement(node) || visited.has(node)) return '';
    visited.add(node);
    if (fromContent && isHiddenForAria(node)) return '';
    const label = node.getAttribute('aria-label');
    const labelledBy = node.getAttribute('aria-labelledby');
    if (labelledBy && !fromContent) {
      const parts = labelledBy.split(/\s+/).map((id) => node.ownerDocument.getElementById(id)).filter(Boolean);
      if (parts.length) return parts.map((part) => textAlternative(part, visited, true)).join(' ');
    }
    if (label && label.trim()) return label;
    const tag = node.nodeName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
      const type = (node.getAttribute('type') || '').toLowerCase();
      if (fromContent) {
        if (tag === 'SELECT') return Array.from(node.selectedOptions).map((o) => o.label).join(' ');
        if (['button', 'submit', 'reset'].includes(type)) return node.value;
        return node.value || '';
      }
      if (['button', 'submit', 'reset'].includes(type)) return node.value || (type === 'submit' ? 'Submit' : type === 'reset' ? 'Reset' : '');
      if (type === 'image') return node.getAttribute('alt') || node.getAttribute('value') || node.getAttribute('title') || 'Submit';
      const labels = node.labels ? Array.from(node.labels) : [];
      if (labels.length) return labels.map((l) => textAlternative(l, visited, true)).join(' ');
      return node.getAttribute('title') || node.getAttribute('placeholder') || '';
    }
    if (tag === 'IMG' || tag === 'AREA') return node.getAttribute('alt') || node.getAttribute('title') || '';
    if (tag === 'FIELDSET' && !fromContent) {
      const legend = node.querySelector(':scope > legend');
      if (legend) return textAlternative(legend, visited, true);
    }
    if (tag === 'TABLE' && !fromContent && node.caption) return textAlternative(node.caption, visited, true);
    const role = ariaRole(node);
    if (fromContent || NAME_FROM_CONTENT.has(role)) {
      let text = '';
      const children = node.shadowRoot ? [node.shadowRoot, ...node.childNodes] : node.childNodes;
      for (const child of children) {
        if (child.nodeType === 11) {
          for (const inner of child.childNodes) text += textAlternative(inner, visited, true);
          continue;
        }
        const block = isElement(child) && !['inline', 'contents'].includes(getComputedStyle(child).display);
        const value = textAlternative(child, visited, true);
        text += block ? ` ${value} ` : value;
      }
      if (normalize(text)) return text;
    }
    return node.getAttribute('title') || '';
  }

  function accessibleName(element) {
    return normalize(textAlternative(element, new Set(), false));
  }

  function ariaChecked(element) {
    if (element.nodeName === 'INPUT' && ['checkbox', 'radio'].includes(element.type)) {
      if (element.indeterminate && element.type === 'checkbox') return 'mixed';
      return element.checked;
    }
    const value = element.getAttribute('aria-checked');
    if (value === 'true') return true;
    if (value === 'mixed') return 'mixed';
    if (value === 'false') return false;
    return undefined;
  }

  function isDisabled(element) {
    if (['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA', 'OPTION', 'OPTGROUP', 'FIELDSET'].includes(element.nodeName)) {
      if (element.matches(':disabled')) return true;
    }
    for (let current = element; current; current = parentElementOrShadowHost(current))
      if (current.getAttribute('aria-disabled') === 'true') return true;
    return false;
  }

  function roleQuery(scope, body) {
    const match = /^([a-zA-Z-]+)(.*)$/s.exec(body.trim());
    if (!match) throw new Error(`Invalid role selector "${body}"`);
    const role = match[1].toLowerCase();
    const filters = [];
    const attrRe = /\[\s*([a-zA-Z-]+)\s*(?:=\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|[^\]\s]+)\s*([is]?))?\s*\]/gy;
    let rest = match[2].trim();
    attrRe.lastIndex = 0;
    let index = 0;
    while (index < rest.length) {
      attrRe.lastIndex = index;
      const attr = attrRe.exec(rest);
      if (!attr) throw new Error(`Invalid role selector "${body}"`);
      filters.push({ name: attr[1], value: attr[2], flag: attr[3] });
      index = attrRe.lastIndex;
      while (rest[index] === ' ') index++;
    }
    let includeHidden = false;
    const tests = [];
    for (const { name, value, flag } of filters) {
      const raw = value === undefined ? 'true' : /^["']/.test(value) ? unquote(value) : value;
      if (name === 'name') {
        let matcher;
        if (value && /^\//.test(value)) matcher = textMatcher(value).match;
        else if (flag === 's' || filters.some((f) => f.name === 'exact' && f.value !== 'false')) matcher = (text) => normalize(text) === normalize(raw);
        else matcher = (text) => normalize(text).toLowerCase().includes(normalize(raw).toLowerCase());
        tests.push((element) => matcher(accessibleName(element)));
      } else if (name === 'exact') {
        continue;
      } else if (name === 'include-hidden') {
        includeHidden = raw !== 'false';
      } else if (name === 'checked') {
        tests.push((element) => String(ariaChecked(element)) === (raw === 'mixed' ? 'mixed' : String(raw !== 'false')));
      } else if (name === 'disabled') {
        tests.push((element) => isDisabled(element) === (raw !== 'false'));
      } else if (name === 'expanded') {
        tests.push((element) => (element.getAttribute('aria-expanded') === 'true') === (raw !== 'false'));
      } else if (name === 'pressed') {
        tests.push((element) => (element.getAttribute('aria-pressed') || 'false') === (raw === 'mixed' ? 'mixed' : String(raw !== 'false')));
      } else if (name === 'selected') {
        tests.push((element) => (element.selected === true || element.getAttribute('aria-selected') === 'true') === (raw !== 'false'));
      } else if (name === 'level') {
        tests.push((element) => String(HEADINGS[element.nodeName] || element.getAttribute('aria-level')) === String(raw));
      } else {
        throw new Error(`Unknown attribute "${name}" in role selector`);
      }
    }
    return composedElements(scope).filter((element) => ariaRole(element) === role
      && (includeHidden || !isHiddenForAria(element)) && tests.every((test) => test(element)));
  }

  function labelQuery(scope, body) {
    const { match } = textMatcher(body);
    return composedElements(scope).filter((element) => {
      if (!['INPUT', 'TEXTAREA', 'SELECT', 'BUTTON', 'METER', 'OUTPUT', 'PROGRESS'].includes(element.nodeName)
        && !element.hasAttribute('aria-labelledby') && !element.hasAttribute('aria-label')) return false;
      if (element.hasAttribute('aria-label') && match(element.getAttribute('aria-label'))) return true;
      const labels = element.labels ? Array.from(element.labels) : [];
      return labels.some((label) => match(elementText(new Map(), label))) || (element.hasAttribute('aria-labelledby') && match(accessibleName(element)));
    });
  }

  // ------------------------------------------------------------- selector
  function splitChain(selector) {
    const parts = [];
    let quote = null, depth = 0, start = 0;
    for (let i = 0; i < selector.length; i++) {
      const ch = selector[i];
      if (quote) {
        if (ch === '\\') i++;
        else if (ch === quote) quote = null;
        continue;
      }
      if (ch === '"' || ch === "'") quote = ch;
      else if (ch === '[' || ch === '(') depth++;
      else if (ch === ']' || ch === ')') depth--;
      else if (depth === 0 && selector.startsWith('>>', i)) {
        parts.push(selector.slice(start, i).trim());
        i++;
        start = i + 1;
      }
    }
    parts.push(selector.slice(start).trim());
    if (parts.some((part) => !part)) throw new Error(`Malformed selector: ${selector}`);
    return parts;
  }

  function parsePart(part) {
    const engine = /^([a-zA-Z_][a-zA-Z0-9_:+*-]*)=(.*)$/s.exec(part);
    if (engine) return { name: engine[1].toLowerCase(), body: engine[2] };
    if (part.startsWith('//') || part.startsWith('..')) return { name: 'xpath', body: part };
    if (part.startsWith('"') || part.startsWith("'")) return { name: 'text', body: part };
    return { name: 'css', body: part };
  }

  function queryPart(scope, name, body) {
    switch (name) {
      case 'css': return cssQuery(scope, body);
      case 'xpath': return xpathQuery(scope, body);
      case 'text': return textQuery(scope, body);
      case 'internal:text': return textQuery(scope, body);
      case 'id': return cssQuery(scope, `[id=${JSON.stringify(unquoteMaybe(body))}]`);
      case 'data-testid': case 'data-test-id': case 'data-test':
        return cssQuery(scope, `[${name}=${JSON.stringify(unquoteMaybe(body))}]`);
      case 'internal:testid': {
        const attr = /^\[\s*([a-zA-Z_-]+)\s*=\s*(.*?)\s*([is]?)\s*\]$/s.exec(body.trim());
        if (!attr) throw new Error(`Invalid testid selector "${body}"`);
        return cssQuery(scope, `[${attr[1]}=${JSON.stringify(unquoteMaybe(attr[2]))}]`);
      }
      case 'role': case 'internal:role': return roleQuery(scope, body);
      case 'internal:label': return labelQuery(scope, body);
      case 'aria-ref': {
        const entry = refs.get(body.trim());
        const element = entry && entry.deref();
        return element && element.isConnected && composedContains(scope, element) ? [element] : [];
      }
      default:
        throw new Error(`Unknown engine "${name}" while parsing selector ${name}=${body}`);
    }
  }

  function unquoteMaybe(text) {
    text = text.trim();
    return /^(["']).*\1$/s.test(text) ? unquote(text) : text;
  }

  // With the vendored Playwright injected script present (`globalThis.__pw`),
  // selectors are parsed and evaluated by exactly the engine upstream uses;
  // the local engines below remain as a fallback.
  function query(selector) {
    if (typeof selector !== 'string' || !selector.trim()) throw new Error('Selector must be a non-empty string');
    if (globalThis.__pw) return globalThis.__pw.querySelectorAll(globalThis.__pw.parseSelector(selector), document);
    let current = [document];
    for (const part of splitChain(selector)) {
      const { name, body } = parsePart(part);
      if (name === 'nth') {
        const n = Number(body);
        if (!Number.isInteger(n)) throw new Error(`Invalid nth selector "${body}"`);
        const element = current[n < 0 ? current.length + n : n];
        current = element ? [element] : [];
        continue;
      }
      if (name === 'visible') {
        const wanted = body.trim() !== 'false';
        current = current.filter((element) => isVisible(element) === wanted);
        continue;
      }
      const next = new Set();
      for (const scope of current) for (const element of queryPart(scope, name, body)) next.add(element);
      current = Array.from(next);
    }
    return current.filter((node) => node.nodeType === 1);
  }

  // ---------------------------------------------------------- inspection
  function strictError(selector, found) {
    if (globalThis.__pw) return globalThis.__pw.strictModeViolationError(globalThis.__pw.parseSelector(selector), found).message;
    const shown = found.slice(0, 10).map((e, i) => `    ${i + 1}) ${preview(e)}`).join('\n');
    return `strict mode violation: locator(${JSON.stringify(selector)}) resolved to ${found.length} elements:\n${shown}`;
  }

  function snapshot(selector, depth, boxes) {
    if (!globalThis.__pw) throw new Error('ARIA snapshot engine is unavailable');
    let root;
    if (selector) {
      const found = query(selector);
      if (found.length > 1) throw new Error(strictError(selector, found));
      if (!found.length) throw new Error(`Selector "${selector}" does not match any element`);
      root = found[0];
    } else {
      root = document.body || document.querySelector('frameset');
      if (!root) return null;
    }
    const options = { mode: 'ai', boxes: !!boxes };
    if (depth !== null && depth !== undefined) options.depth = depth;
    return globalThis.__pw.ariaSnapshot(root, options);
  }

  function preview(element) {
    if (globalThis.__pw) return globalThis.__pw.previewNode(element);
    const attrs = Array.from(element.attributes).slice(0, 6)
      .map((a) => a.value === '' ? ` ${a.name}` : ` ${a.name}="${a.value.slice(0, 50)}"`).join('');
    const tag = element.nodeName.toLowerCase();
    const text = normalize(element.textContent || '').slice(0, 50);
    if (!element.firstChild) return `<${tag}${attrs}/>`;
    return `<${tag}${attrs}>${element.children.length ? '…' : text}</${tag}>`;
  }

  function isVisible(element) {
    const style = getComputedStyle(element);
    if (style.display === 'contents') {
      for (let child = element.firstChild; child; child = child.nextSibling) {
        if (child.nodeType === 1 && isVisible(child)) return true;
        if (child.nodeType === 3 && child.nodeValue.trim()) {
          const range = document.createRange();
          range.selectNodeContents(child);
          const rect = range.getBoundingClientRect();
          if (rect.width > 0 && rect.height > 0) return true;
        }
      }
      return false;
    }
    if (element.checkVisibility && !element.checkVisibility()) return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && style.visibility === 'visible';
  }

  function isEditable(element) {
    if (isDisabled(element)) return false;
    if (['INPUT', 'TEXTAREA', 'SELECT'].includes(element.nodeName)) return !element.hasAttribute('readonly');
    if (element.isContentEditable) return true;
    return element.getAttribute('aria-readonly') !== 'true';
  }

  // Elements that delegate their form behaviour to a labelled control.
  function retarget(element, behavior) {
    if (behavior === 'none') return element;
    if (!element.matches('input, textarea, select') && !element.isContentEditable) {
      const label = element.closest('label');
      if (label && label.control) return label.control;
    }
    return element;
  }

  function check(element, states) {
    for (const state of states) {
      if (!element.isConnected) return 'element is not attached to the DOM';
      if (state === 'visible' && !isVisible(element)) return 'element is not visible';
      if (state === 'enabled' && isDisabled(element)) return 'element is not enabled';
      if (state === 'editable' && !isEditable(element)) return 'element is not editable';
    }
    return 'ok';
  }

  function deepElementFromPoint(x, y) {
    let element = document.elementFromPoint(x, y);
    while (element && element.shadowRoot) {
      const inner = element.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === element) break;
      element = inner;
    }
    return element;
  }

  function hitTarget(element, x, y) {
    const hit = deepElementFromPoint(x, y);
    if (!hit) return 'no element at the click point';
    if (composedContains(element, hit)) return 'ok';
    const label = hit.closest && hit.closest('label');
    if (label && label.control === element) return 'ok';
    return `${preview(hit)} intercepts pointer events`;
  }

  const SET_VALUE_TYPES = new Set(['color', 'date', 'time', 'datetime-local', 'month', 'range', 'week']);
  const TYPE_INTO_TYPES = new Set(['', 'email', 'number', 'password', 'search', 'tel', 'text', 'url']);

  // Prepare `fill`: either set the value directly or focus and select all
  // existing content so that the next insertText replaces it.
  function fillPrepare(element, value) {
    if (element.nodeName === 'INPUT') {
      const type = (element.getAttribute('type') || '').toLowerCase();
      if (SET_VALUE_TYPES.has(type)) {
        value = value.trim();
        element.focus();
        element.value = value;
        if (element.value !== value) throw new Error('Malformed value');
        element.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return 'done';
      }
      if (!TYPE_INTO_TYPES.has(type)) throw new Error(`Input of type "${type}" cannot be filled`);
      if (type === 'number' && value.trim() !== '' && Number.isNaN(Number(value.trim())))
        throw new Error('Cannot type text into input[type=number]');
      element.focus();
      element.select();
      return 'type';
    }
    if (element.nodeName === 'TEXTAREA') {
      element.focus();
      element.select();
      return 'type';
    }
    if (element.isContentEditable) {
      element.focus();
      const range = element.ownerDocument.createRange();
      range.selectNodeContents(element);
      const selection = element.ownerDocument.defaultView.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      return 'type';
    }
    throw new Error('Element is not an <input>, <textarea> or [contenteditable] element');
  }

  function deepActiveElement() {
    let active = document.activeElement;
    while (active && active.shadowRoot && active.shadowRoot.activeElement) active = active.shadowRoot.activeElement;
    return active;
  }

  // Focus for typing. Like Playwright, an <input> that was not already
  // focused gets its caret reset to the start before keys are typed.
  function focus(element) {
    if (!element.isConnected) throw new Error('Element is not attached to the DOM');
    const wasFocused = deepActiveElement() === element;
    element.focus();
    if (!wasFocused && element.nodeName === 'INPUT') {
      try { element.setSelectionRange(0, 0); } catch (error) { /* type without selection API */ }
    }
    return 'ok';
  }

  function checkedState(element) {
    const state = ariaChecked(element);
    if (state === undefined) throw new Error('Not a checkbox or radio button');
    return state === true;
  }

  function selectOptions(element, labels) {
    if (element.nodeName !== 'SELECT') throw new Error('Element is not a <select> element');
    const options = Array.from(element.options);
    const chosen = [];
    for (const label of labels) {
      const option = options.find((candidate) => candidate.label === label);
      if (!option) return 'missing';
      chosen.push(option);
    }
    if (chosen.length > 1 && !element.multiple) throw new Error('Error: Element is not a multiple <select> element');
    if (element.multiple) for (const option of options) option.selected = chosen.includes(option);
    else if (chosen.length) element.value = chosen[0].value;
    else element.selectedIndex = -1;
    element.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return 'ok';
  }

  function rememberRef(element) {
    const ref = `e${++refGeneration}`;
    refs.set(ref, new WeakRef(element));
    return ref;
  }

  async function stableRect(element) {
    if (!element.isConnected) return false;
    const before = element.getBoundingClientRect();
    await new Promise((resolve) => {
      let done = false;
      const finish = () => { if (!done) { done = true; resolve(); } };
      requestAnimationFrame(() => requestAnimationFrame(finish));
      setTimeout(finish, 50);
    });
    const after = element.getBoundingClientRect();
    return before.x === after.x && before.y === after.y && before.width === after.width && before.height === after.height;
  }

  function pageRect(element) {
    const rect = element.getBoundingClientRect();
    return [rect.x + window.scrollX, rect.y + window.scrollY, rect.width, rect.height];
  }

  globalThis.__rsx = {
    snapshot, strictError, pageRect,
    stableRect, outerHTML: (element) => element.outerHTML,
    query, preview, isVisible, check, hitTarget, retarget, fillPrepare, focus, checkedState, selectOptions,
    ariaRole, accessibleName, isHiddenForAria, ariaChecked, isDisabled, normalize, composedElements,
    rememberRef, refs, resetRefs: () => { refs.clear(); refGeneration = 0; },
    elementText: (element) => elementText(new Map(), element),
  };
  return true;
})()
