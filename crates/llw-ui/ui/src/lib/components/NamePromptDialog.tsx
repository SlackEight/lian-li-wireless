import { useEffect, useState } from 'react';

/**
 * In-app themed name prompt (ConfirmDialog idiom + an input). Enter confirms
 * when non-blank; Escape and overlay clicks cancel. Extracted from the
 * Cooling screen (D3) for reuse by preset saving (C4).
 */
export default function NamePromptDialog({
  title,
  confirmLabel,
  placeholder,
  onConfirm,
  onCancel,
}: {
  title: string;
  confirmLabel: string;
  placeholder: string;
  onConfirm: (name: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState('');
  const blank = name.trim() === '';

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onCancel]);

  return (
    <div className="dialog-overlay" onClick={onCancel}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="dialog-title">{title}</div>
        <input
          className="dialog-input"
          placeholder={placeholder}
          autoFocus
          value={name}
          onChange={(e) => setName(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !blank) onConfirm(name);
          }}
        />
        <div className="dialog-actions">
          <button type="button" className="btn-quiet" onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className="btn-accent"
            disabled={blank}
            onClick={() => onConfirm(name)}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
