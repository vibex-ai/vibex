import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform === "linux") {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const dataHome = resolve(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"));
  const appId = "dev.vibex.desktop.preview";
  const desktopDir = join(dataHome, "applications");
  const iconSizes = [16, 24, 32, 48, 64, 128, 256, 1024];

  mkdirSync(desktopDir, { recursive: true });
  for (const size of iconSizes) {
    const iconDir = join(dataHome, "icons", "hicolor", `${size}x${size}`, "apps");
    const sourceName = size === 1024 ? "icon.png" : `icon-${size}.png`;
    mkdirSync(iconDir, { recursive: true });
    copyFileSync(
      join(root, "apps", "desktop", "assets", "app-icons", sourceName),
      join(iconDir, `${appId}.png`)
    );
  }
  const iconPath = join(dataHome, "icons", "hicolor", "256x256", "apps", `${appId}.png`);
  const desktopPath = join(desktopDir, `${appId}.desktop`);
  writeFileSync(
    desktopPath,
    `[Desktop Entry]\nName=Vibex Preview\nComment=Vibex development desktop application\nExec=${join(root, "target", "debug", "vibex-desktop")}\nIcon=${iconPath}\nNoDisplay=true\nStartupWMClass=${appId}\nTerminal=false\nType=Application\n`
  );
}
