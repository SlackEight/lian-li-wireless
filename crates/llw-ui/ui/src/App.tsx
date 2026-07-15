import { useState } from 'react';
import { type Section } from './lib/sections.js';
import { useStatus } from './lib/stores/useStatus.js';
import Sidebar from './lib/components/Sidebar.js';
import DaemonBanner from './lib/components/DaemonBanner.js';
import ToastArea from './lib/components/ToastArea.js';
import Health from './lib/sections/Health.js';
import Devices from './lib/sections/Devices.js';
import Lighting from './lib/sections/Lighting.js';
import Cooling from './lib/sections/Cooling.js';

export default function App() {
  const [active, setActive] = useState<Section>('Health');

  const { daemonReachable } = useStatus();
  const daemonUnreachable = !daemonReachable;

  return (
    <div className="shell">
      <Sidebar active={active} onSelect={setActive} />

      <div className="main-column">
        <DaemonBanner visible={daemonUnreachable} />

        <main className={`content${daemonUnreachable ? ' dimmed' : ''}`}>
          {active === 'Health' && <Health />}
          {active === 'Devices' && <Devices />}
          {active === 'Lighting' && <Lighting />}
          {active === 'Cooling' && <Cooling />}
        </main>
      </div>

      <ToastArea />
    </div>
  );
}
