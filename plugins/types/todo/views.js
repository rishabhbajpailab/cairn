// Type plugin renderers: framework-agnostic mount functions (ADR 0006).
// Each returns { update(item), destroy() }. ctx: { patch, action, link, navigate, theme }.

function el(tag, attrs = {}, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
    else if (k === 'class') n.className = v;
    else n.setAttribute(k, v);
  }
  n.append(...children);
  return n;
}

function due(item) {
  const d = item.payload.due;
  return d ? new Date(d).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '';
}

function render(root, item, ctx, compact) {
  root.replaceChildren();
  const box = el('input', { type: 'checkbox', onchange: (e) => ctx.patch({ done: e.target.checked }) });
  box.checked = !!item.payload.done;
  const text = el('span', { class: item.payload.done ? 'cairn-done' : '' }, item.payload.text);
  root.append(el('label', { class: 'cairn-todo' }, box, text));
  if (item.payload.due) root.append(el('small', { class: 'cairn-meta' }, `due ${due(item)}`));
  if (!compact && item.payload.notes) root.append(el('p', {}, item.payload.notes));
}

const mount = (compact) => (root, item, ctx) => {
  render(root, item, ctx, compact);
  return { update: (next) => render(root, next, ctx, compact), destroy: () => root.replaceChildren() };
};

export default {
  card: mount(true),
  node: mount(true),
  detail: mount(false),
  editor(root, item, ctx) {
    const form = el('form', { class: 'cairn-form' });
    const text = el('input', { name: 'text', value: item.payload.text ?? '' });
    const dueIn = el('input', { name: 'due', type: 'datetime-local' });
    if (item.payload.due) dueIn.value = item.payload.due.slice(0, 16);
    const notes = el('textarea', { name: 'notes' }, item.payload.notes ?? '');
    form.append(text, dueIn, notes, el('button', { type: 'submit' }, 'Save'));
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      const patch = { text: text.value, notes: notes.value };
      if (dueIn.value) patch.due = new Date(dueIn.value).toISOString();
      ctx.patch(patch);
    });
    root.replaceChildren(form);
    return { update() {}, destroy: () => root.replaceChildren() };
  },
};
