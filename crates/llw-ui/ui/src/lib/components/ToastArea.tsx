import { useToasts, toastStore } from '../stores/useToasts.js';

/**
 * Bottom-right toast stack. Each toast auto-dismisses after 6s (the store's
 * timer) or on click. Fixed-positioned so it stays usable even while the
 * content dims behind the daemon-unreachable banner.
 */
export default function ToastArea() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;

  return (
    <div className="toast-area">
      {toasts.map((toast) => (
        <button
          key={toast.id}
          type="button"
          className={`toast ${toast.kind}`}
          role="alert"
          title="dismiss"
          onClick={() => toastStore.dismiss(toast.id)}
        >
          {toast.message}
        </button>
      ))}
    </div>
  );
}
