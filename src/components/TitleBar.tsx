import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { IS_MAC } from "../dictation";
import { IconButton } from "../ui";

/// The main window's own title bar: a strip that drags the window, a double click maximizes it.
/// On Windows and Linux the system bar is off (`decorations` in tauri.conf.json) and the strip
/// holds the window buttons in the app's style; on macOS the page runs under a transparent system
/// bar (`titleBarStyle: Overlay` in tauri.macos.conf.json) whose own buttons stay in the corner.
/// Closing hides the window to the tray, like the system close button.
export function TitleBar() {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    const check = () => void appWindow.isMaximized().then(setMaximized, () => undefined);
    check();
    const stop = appWindow.onResized(check);
    return () => void stop.then((unlisten) => unlisten());
  }, []);

  if (IS_MAC) return <div className="title-bar" data-tauri-drag-region="deep" />;
  const appWindow = getCurrentWindow();
  return (
    <div className="title-bar" data-tauri-drag-region="deep">
      <IconButton icon="minimize" label={t("window.minimize")} onClick={() => void appWindow.minimize()} />
      <IconButton
        icon={maximized ? "restore" : "maximize"}
        label={t(maximized ? "window.restore" : "window.maximize")}
        onClick={() => void appWindow.toggleMaximize()}
      />
      <IconButton icon="close" label={t("window.close")} onClick={() => void appWindow.close()} />
    </div>
  );
}
