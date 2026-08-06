import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform === "linux") {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const dataHome = resolve(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"));
  const appId = "dev.vibex.desktop.preview";
  const iconDir = join(dataHome, "icons", "hicolor", "1024x1024", "apps");
  const desktopDir = join(dataHome, "applications");
  const iconPath = join(iconDir, `${appId}.png`);
  const desktopPath = join(desktopDir, `${appId}.desktop`);

  mkdirSync(iconDir, { recursive: true });
  mkdirSync(desktopDir, { recursive: true });
  copyFileSync(join(root, "apps", "desktop", "assets", "app-icons", "icon.png"), iconPath);
  writeFileSync(
    desktopPath,
    `[Desktop Entry]\nName=Vibex Preview\nComment=Vibex development desktop application\nExec=${join(root, "target", "debug", "vibex-desktop")}\nIcon=${appId}\nNoDisplay=true\nStartupWMClass=${appId}\nTerminal=false\nType=Application\n`
  );
}
