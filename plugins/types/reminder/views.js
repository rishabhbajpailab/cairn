// Minimal renderers for `reminder`. Replace with richer ones as the UI grows (ADR 0006).
// Missing renderers (e.g. editor) fall back to the generic JSON renderer in web/.

function render(root, item) {
  root.replaceChildren();
  const title = document.createElement('strong');
  title.textContent = item.payload.message ?? item.title;
  const meta = document.createElement('small');
  meta.className = 'cairn-meta';
  meta.textContent = `${item.payload.state ?? "pending"} · ${new Date(item.payload.fire_at).toLocaleString()}`;
  root.append(title, meta);
}

const mount = (root, item) => {
  render(root, item);
  return { update: (next) => render(root, next), destroy: () => root.replaceChildren() };
};

export default { card: mount, node: mount, detail: mount };
