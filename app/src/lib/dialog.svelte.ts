// 全局对话框（替代浏览器原生 confirm/prompt）。
export interface DialogState {
  kind: "confirm" | "prompt";
  title: string;
  message: string;
  label: string;
  value: string;
  confirmText: string;
  danger: boolean;
}

export const dialogBox = $state<{ current: (DialogState & { resolve: (v: unknown) => void }) | null }>({ current: null });

export function confirmDialog(opts: { title: string; message: string; danger?: boolean; confirmText?: string }): Promise<boolean> {
  return new Promise((resolve) => {
    dialogBox.current = {
      kind: "confirm",
      title: opts.title,
      message: opts.message,
      label: "",
      value: "",
      confirmText: opts.confirmText ?? "确定",
      danger: opts.danger ?? false,
      resolve: (v) => resolve(Boolean(v)),
    };
  });
}

export function promptDialog(opts: { title: string; label: string; initial?: string }): Promise<string | null> {
  return new Promise((resolve) => {
    dialogBox.current = {
      kind: "prompt",
      title: opts.title,
      message: "",
      label: opts.label,
      value: opts.initial ?? "",
      confirmText: "确定",
      danger: false,
      resolve: (v) => resolve(typeof v === "string" ? v : null),
    };
  });
}

/** Host calls this with the final value (boolean for confirm, string|null for prompt). */
export function settleDialog(v: unknown) {
  dialogBox.current?.resolve(v);
  dialogBox.current = null;
}
