function inspectArtifact(input) {
  const prompts = Array.isArray(input.artifact.prompts) ? input.artifact.prompts : [];
  const byId = new Map();
  const byName = new Map();
  const parsed = new Map();

  function normalize(value) {
    return String(value || '').trim().toLowerCase().replace(/^[^\p{L}\p{N}]+/u, '').replace(/\s+/g, ' ');
  }

  function directives(content) {
    const result = {
      groups: [], exclusive: [], categories: [], maxCategories: [],
      conflicts: [], warnings: [], deprecated: []
    };
    const blocks = String(content || '').matchAll(/\{\{\/\/([\s\S]*?)\}\}/g);
    for (const block of blocks) {
      for (const source of block[1].split(/\r?\n/)) {
        const line = source.trim();
        const split = line.indexOf(' ');
        const name = split < 0 ? line : line.slice(0, split);
        const value = split < 0 ? '' : line.slice(split + 1).trim();
        const list = () => value.split(',').map(item => item.trim()).filter(Boolean);
        if (name === '@mutual-exclusive-group' || name === '@exclusive-with-category') result.groups.push(value);
        else if (name === '@exclusive-with') result.exclusive.push(...list());
        else if (name === '@category') result.categories.push(...list());
        else if (name === '@max-one-per-category') result.maxCategories.push(value);
        else if (name === '@conflicts-with') result.conflicts.push(...list());
        else if (name === '@warning' && value) result.warnings.push(value);
        else if (name === '@deprecated' && value) result.deprecated.push(value);
      }
    }
    return result;
  }

  for (const prompt of prompts) {
    if (!prompt || typeof prompt.identifier !== 'string') continue;
    byId.set(prompt.identifier, prompt);
    byName.set(normalize(prompt.identifier), prompt);
    if (typeof prompt.name === 'string') byName.set(normalize(prompt.name), prompt);
    parsed.set(prompt.identifier, directives(prompt.content));
  }

  function resolve(reference) {
    return byId.get(reference) || byName.get(normalize(reference));
  }

  const constraints = [];
  const diagnostics = [];
  const namedGroups = new Map();
  const categoryLimits = new Set();

  for (const prompt of prompts) {
    if (!prompt || !parsed.has(prompt.identifier)) continue;
    const data = parsed.get(prompt.identifier);
    for (const group of data.groups) {
      if (!group) continue;
      if (!namedGroups.has(group)) namedGroups.set(group, []);
      namedGroups.get(group).push(prompt.identifier);
    }
    for (const reference of data.exclusive) {
      const target = resolve(reference);
      if (target) constraints.push({ kind: 'exclusive-pair', name: reference, members: [prompt.identifier, target.identifier] });
      else diagnostics.push({ identifier: prompt.identifier, severity: 'warning', kind: 'unresolved-reference', message: `Could not resolve exclusive prompt reference "${reference}".` });
    }
    for (const category of data.maxCategories) if (category) categoryLimits.add(category);
    for (const reference of data.conflicts) {
      const target = resolve(reference);
      if (target) diagnostics.push({ identifier: prompt.identifier, severity: 'warning', kind: 'conflict', target: target.identifier, message: `"${prompt.name || prompt.identifier}" may conflict with "${target.name || target.identifier}".` });
      else diagnostics.push({ identifier: prompt.identifier, severity: 'warning', kind: 'unresolved-reference', message: `Could not resolve conflicting prompt reference "${reference}".` });
    }
    for (const message of data.warnings) diagnostics.push({ identifier: prompt.identifier, severity: 'warning', kind: 'warning', message });
    for (const message of data.deprecated) diagnostics.push({ identifier: prompt.identifier, severity: 'warning', kind: 'deprecated', message: `"${prompt.name || prompt.identifier}" is deprecated. ${message}` });
  }

  for (const [name, members] of namedGroups) {
    if (members.length > 1) constraints.push({ kind: 'named-group', name, members: [...new Set(members)] });
  }
  for (const category of categoryLimits) {
    const members = prompts.filter(prompt => parsed.has(prompt.identifier) && parsed.get(prompt.identifier).categories.includes(category)).map(prompt => prompt.identifier);
    if (members.length > 1) constraints.push({ kind: 'category-limit', name: category, members: [...new Set(members)] });
  }

  stcli.output({ constraints, diagnostics });
}
