<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte';
  import type { Item } from '../types';
  import ItemView from './ItemView.svelte';

  let { data }: { data: { item: Item } } = $props();
</script>

<div class="node">
  <Handle type="target" position={Position.Left} />
  <span class="badge">{data.item.type}</span>
  <ItemView item={data.item} kind="node" />
  <Handle type="source" position={Position.Right} />
</div>
