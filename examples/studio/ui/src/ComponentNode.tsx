import { Handle, Position, type NodeProps, type Node } from "@xyflow/react";
import { Boxes, Cpu, Globe, Server, Trash2 } from "lucide-react";
import { shortIface, shortSize, type Surface } from "./api";

export type ComponentNodeData = {
  componentId: string;
  surface: Surface;
  /// Interfaces this node still needs (from the server's plan) — drives the
  /// red/green dot on each import handle.
  gaps: Set<string>;
  onRemove: (id: string) => void;
};

export type ComponentFlowNode = Node<ComponentNodeData, "component">;

/// One handle per interface, which is what makes an illegal edge undrawable:
/// xyflow can only connect a source handle to a target handle, and the handle
/// ids ARE the interface names, so the geometry enforces the contract.
export default function ComponentNode({ data, selected }: NodeProps<ComponentFlowNode>) {
  const { componentId, surface, gaps, onRemove } = data;
  const servesHttp = surface.exports.some((e) => e.name === "incoming-handler");
  const pureCompute = surface.host_imports.length === 0 && surface.imports.length === 0;
  const Icon = servesHttp ? Globe : pureCompute ? Cpu : Boxes;

  return (
    <div
      className={`w-64 rounded-lg border bg-card text-card-foreground shadow-sm ${
        selected ? "ring-2 ring-primary" : ""
      }`}
    >
      <div className="flex items-center gap-2 border-b px-3 py-2">
        <Icon className="size-4 shrink-0 text-primary" />
        <span className="truncate text-sm font-medium" title={componentId}>
          {componentId}
        </span>
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
          {shortSize(surface.size_bytes)}
        </span>
        <button
          className="shrink-0 text-muted-foreground hover:text-red-600"
          title="remove from canvas"
          onClick={(e) => {
            e.stopPropagation();
            onRemove(componentId);
          }}
        >
          <Trash2 className="size-3.5" />
        </button>
      </div>

      {/* exports: source handles on the right */}
      {surface.exports.length > 0 && (
        <div className="border-b py-1">
          {surface.exports.map((e) => (
            <div key={e.raw} className="relative flex items-center justify-end gap-2 px-3 py-1">
              <span className="truncate text-[11px] text-muted-foreground" title={e.raw}>
                {shortIface(e)}
                {e.version && <span className="ml-1 opacity-50">{e.version}</span>}
              </span>
              <Handle
                id={e.raw}
                type="source"
                position={Position.Right}
                className="!size-2.5 !border-2 !border-card !bg-emerald-500"
              />
            </div>
          ))}
        </div>
      )}

      {/* composable imports: target handles on the left */}
      {surface.imports.map((i) => {
        const open = gaps.has(i.raw);
        return (
          <div key={i.raw} className="relative flex items-center gap-2 px-3 py-1">
            <Handle
              id={i.raw}
              type="target"
              position={Position.Left}
              className={`!size-2.5 !border-2 !border-card ${open ? "!bg-amber-500" : "!bg-sky-500"}`}
            />
            <span className="truncate text-[11px]" title={i.raw}>
              {shortIface(i)}
              {i.version && <span className="ml-1 text-muted-foreground opacity-60">{i.version}</span>}
            </span>
            {open && <span className="ml-auto shrink-0 text-[10px] text-amber-600">needs</span>}
          </div>
        );
      })}

      {/* host imports: no handle, on purpose — no component can satisfy these */}
      {surface.host_imports.length > 0 && (
        <div className="flex flex-wrap items-center gap-1 border-t px-3 py-2">
          <Server className="size-3 text-muted-foreground" />
          <span className="text-[10px] text-muted-foreground">
            host: {surface.host_imports.length} interface
            {surface.host_imports.length === 1 ? "" : "s"}
          </span>
          <span
            className="truncate text-[10px] text-muted-foreground opacity-60"
            title={surface.host_imports.map((h) => h.raw).join("\n")}
          >
            {[...new Set(surface.host_imports.map((h) => `${h.namespace}:${h.pkg}`))].join(" ")}
          </span>
        </div>
      )}
    </div>
  );
}
