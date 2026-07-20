import { Fragment } from 'react';
import { useStatus } from '../stores/useStatus.js';
import {
  sliceToFanCount,
  type DeviceStatus,
  type LinkStatus,
  type Telemetry,
} from '../stores/status.js';

function LinkCard({ link, txWedged }: { link: LinkStatus | null; txWedged: boolean }) {
  return (
    <div className={`card${txWedged ? ' card-danger' : ''}`}>
      <div className="card-title">Link</div>

      {txWedged && (
        <div className="wedge-banner" role="alert">
          <span className="wedge-dot" aria-hidden="true"></span>
          TX wedged — transmit path stalled
        </div>
      )}

      {link ? (
        <div className="stat-row">
          <div className="stat">
            <span className="stat-value mac">{link.master_mac}</span>
            <span className="stat-label">master</span>
          </div>
          <div className="stat">
            <span className="stat-value">{link.channel}</span>
            <span className="stat-label">channel</span>
          </div>
        </div>
      ) : (
        <div className="acquiring">
          <span className="acquiring-dot" aria-hidden="true"></span>
          acquiring link…
        </div>
      )}
    </div>
  );
}

function ReliabilityCard({ reliability }: { reliability: Telemetry }) {
  return (
    <div className="card">
      <div className="card-title">Reliability</div>
      <div className="stat-row">
        <div className="stat">
          <span className="stat-value">{reliability.total_dropouts}</span>
          <span className="stat-label">dropouts</span>
        </div>
        <div className="stat">
          <span className="stat-value">{reliability.total_tier1}</span>
          <span className="stat-label">tier-1</span>
        </div>
        <div className="stat">
          <span className="stat-value">{reliability.total_tier2}</span>
          <span className="stat-label">tier-2</span>
        </div>
        <div className="stat">
          <span className="stat-value">{reliability.failed_tier1_streak}</span>
          <span className="stat-label">failed streak</span>
        </div>
        <div className="stat">
          <span
            className={
              (reliability.total_surges ?? 0) > 0 ? 'stat-value stat-warn' : 'stat-value'
            }
            title={
              reliability.last_surge_peak_rpm
                ? `last peak ${reliability.last_surge_peak_rpm} rpm`
                : undefined
            }
          >
            {reliability.total_surges ?? 0}
          </span>
          <span className="stat-label">fan surges</span>
        </div>
        <div className="stat">
          <span
            className={
              (reliability.total_stalls ?? 0) > 0 ? 'stat-value stat-danger' : 'stat-value'
            }
          >
            {reliability.total_stalls ?? 0}
          </span>
          <span className="stat-label">stalls</span>
        </div>
      </div>
    </div>
  );
}

function SyncBadge({ inSync }: { inSync: boolean | null }) {
  if (inSync === true) return <span className="badge ok">in sync</span>;
  if (inSync === false) return <span className="badge warn">syncing</span>;
  return <span className="badge muted">no effect</span>;
}

// PWM values are raw 0–255 bytes on the wire; people read percentages.
function pwmPercent(raw: number): string {
  return `${Math.round((raw / 255) * 100)}%`;
}

function DeviceSyncCard({ device }: { device: DeviceStatus }) {
  const fans = sliceToFanCount(device.rpm, device.fan_count).map((rpm, i) => ({
    rpm,
    desired: device.desired_pwm[i],
    readback: device.readback_pwm[i],
  }));

  return (
    <div className="card device-sync-card">
      <div className="device-sync-head">
        <div>
          <div className="device-kind">{device.kind}</div>
          <div className="device-mac mac">{device.mac}</div>
        </div>
        <SyncBadge inSync={device.rgb_in_sync} />
      </div>

      <div className="fan-table">
        <span className="fan-cell head name">fan</span>
        <span className="fan-cell head">desired</span>
        <span className="fan-cell head">readback</span>
        <span className="fan-cell head">rpm</span>
        {fans.map((fan, i) => (
          <Fragment key={i}>
            <span className="fan-cell name">{i + 1}</span>
            <span className="fan-cell">{pwmPercent(fan.desired)}</span>
            <span className="fan-cell">{pwmPercent(fan.readback)}</span>
            <span className="fan-cell">{fan.rpm}</span>
          </Fragment>
        ))}
      </div>

      <div className="device-foot">
        <span className="foot-label">dropout streak</span>
        <span className="foot-value">{device.dropout_streak}</span>
      </div>
    </div>
  );
}

export default function Health() {
  const { data } = useStatus();

  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Health</h1>
        <p className="section-subtitle">Link quality, dropouts, and sync status</p>
      </header>

      {data === null ? (
        <div className="card-muted placeholder-card">
          <span>Waiting for daemon data</span>
          <span className="hint">polling status…</span>
        </div>
      ) : (
        <>
          <div className="health-grid">
            <LinkCard link={data.link} txWedged={data.tx_wedged} />
            <ReliabilityCard reliability={data.reliability} />
          </div>

          {data.devices.length === 0 ? (
            <div className="card-muted placeholder-card">
              <span>No configured devices</span>
              <span className="hint">bind one from Devices</span>
            </div>
          ) : (
            <div className="device-sync-grid">
              {data.devices.map((device) => (
                <DeviceSyncCard key={device.mac} device={device} />
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}
