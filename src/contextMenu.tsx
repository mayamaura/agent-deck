import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent } from "react";

/** 右クリックメニューの1項目。"separator" は区切り線(破壊的な項目の前に置く)。 */
export type MenuItem =
  | "separator"
  | { label: string; onClick: () => void; danger?: boolean };

interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

/**
 * 右クリックメニュー(docs/requirements.md §3.6)。openMenu を対象要素の onContextMenu に
 * 渡し、menu を JSX の末尾に置く。テキスト入力欄には付けない(ネイティブの
 * コピー/貼り付けメニューを残す)。
 */
export function useContextMenu() {
  const [state, setState] = useState<MenuState | null>(null);

  function openMenu(e: MouseEvent, items: MenuItem[]) {
    e.preventDefault();
    e.stopPropagation();
    setState({ x: e.clientX, y: e.clientY, items });
  }

  const menu = state ? <ContextMenu state={state} onClose={() => setState(null)} /> : null;
  return { menu, openMenu };
}

function ContextMenu({ state, onClose }: { state: MenuState; onClose: () => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ x: state.x, y: state.y });

  // 画面端ではみ出す分だけ内側へ寄せる(ネイティブメニューと同じ挙動)。
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    setPos({
      x: Math.max(0, Math.min(state.x, window.innerWidth - width - 4)),
      y: Math.max(0, Math.min(state.y, window.innerHeight - height - 4)),
    });
  }, [state]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="context-menu-overlay"
      onClick={onClose}
      onContextMenu={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <div ref={ref} className="context-menu" style={{ left: pos.x, top: pos.y }} role="menu">
        {state.items.map((item, i) =>
          item === "separator" ? (
            <hr key={i} />
          ) : (
            <button
              key={i}
              type="button"
              role="menuitem"
              className={item.danger ? "danger" : undefined}
              onClick={() => {
                onClose();
                item.onClick();
              }}
            >
              {item.label}
            </button>
          ),
        )}
      </div>
    </div>
  );
}
