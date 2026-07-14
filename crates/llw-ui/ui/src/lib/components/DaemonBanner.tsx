interface Props {
  visible: boolean;
  message?: string;
}

export default function DaemonBanner({ visible, message = 'daemon unreachable — retrying' }: Props) {
  if (!visible) return null;
  return (
    <div className="daemon-banner" role="status" aria-live="polite">
      <span className="banner-dot" aria-hidden="true"></span>
      <span className="banner-text">{message}</span>
    </div>
  );
}
