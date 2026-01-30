import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import "./App.css";

function pad2(value: number) {
  return value.toString().padStart(2, "0");
}

function formatDuration(totalSeconds: number) {
  const clamped = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(clamped / 3600);
  const minutes = Math.floor((clamped % 3600) / 60);
  const seconds = clamped % 60;
  return `${pad2(hours)}:${pad2(minutes)}:${pad2(seconds)}`;
}

function formatUsage(totalSeconds: number) {
  const clamped = Math.max(0, Math.floor(totalSeconds));
  const days = Math.floor(clamped / 86400);
  const hours = Math.floor((clamped % 86400) / 3600);
  const minutes = Math.floor((clamped % 3600) / 60);
  if (days > 0) {
    return `${days} 天 ${hours} 小时`;
  }
  if (hours > 0) {
    return `${hours} 小时 ${minutes} 分钟`;
  }
  return `${minutes} 分钟`;
}

function App() {
  const isLockWindow =
    new URLSearchParams(window.location.search).get("lockscreen") === "1";
  const [now, setNow] = useState(new Date());
  const [sessionStart] = useState(() => Date.now());
  const [filterEnabled, setFilterEnabled] = useState(true);
  const [filterStrength, setFilterStrength] = useState(30);
  const [colorTemp, setColorTemp] = useState(4700);
  const [restEnabled, setRestEnabled] = useState(true);
  const [restMinutes, setRestMinutes] = useState(30);
  const [restDuration, setRestDuration] = useState(1);
  const [allowEscExit, setAllowEscExit] = useState(true);
  const [showLockScreen, setShowLockScreen] = useState(false);
  const [activePreset, setActivePreset] = useState("智能");
  const [nextRestAt, setNextRestAt] = useState<Date | null>(null);
  const [restEndAt, setRestEndAt] = useState<Date | null>(null);
  const [restPaused, setRestPaused] = useState(false);
  const [restPausedRemaining, setRestPausedRemaining] = useState<number | null>(
    null,
  );
  const [lockPayload, setLockPayload] = useState({
    timeText: "--:--",
    dateText: "",
    restCountdown: "00:00:00",
    restPaused: false,
    allowEscExit: true,
  });
  const [lockEndAtMs, setLockEndAtMs] = useState<number | null>(null);
  const [lockPausedLocal, setLockPausedLocal] = useState(false);
  const [lockRemainingLocal, setLockRemainingLocal] = useState(0);
  const [lockBackgroundUrl, setLockBackgroundUrl] = useState<string | null>(
    null,
  );
  const [lockWallpaperHistory, setLockWallpaperHistory] = useState<string[]>(
    [],
  );
  const [lockWallpaperIndex, setLockWallpaperIndex] = useState(0);

  const presets = useMemo(
    () => ({
      智能: {
        day: { temp: 4700, strength: 30 },
        night: { temp: 3400, strength: 30 },
      },
      办公: {
        day: { temp: 5200, strength: 50 },
        night: { temp: 4700, strength: 60 },
      },
      影视: {
        day: { temp: 5600, strength: 45 },
        night: { temp: 5200, strength: 55 },
      },
      游戏: {
        day: { temp: 6000, strength: 35 },
        night: { temp: 5600, strength: 45 },
      },
    }),
    [],
  );

  const isDaytime = now.getHours() >= 6 && now.getHours() < 18;
  const resolvePreset = useCallback(
    (preset: keyof typeof presets) => {
      const config = presets[preset];
      if (!config) {
        return { temp: 4700, strength: 30 };
      }
      if (preset === "智能") {
        return isDaytime ? config.day : config.night;
      }
      return config.day;
    },
    [isDaytime, presets],
  );

  useEffect(() => {
    if (activePreset !== "智能") return;
    const next = resolvePreset("智能");
    setFilterStrength(next.strength);
    setColorTemp(next.temp);
  }, [activePreset, resolvePreset]);

  const handleStartRest = useCallback(() => {
    const endAt = new Date(Date.now() + restDuration * 60 * 1000);
    setRestPaused(false);
    setRestPausedRemaining(null);
    setRestEndAt(endAt);
    setShowLockScreen(true);
  }, [restDuration]);

  const handleExitRest = useCallback(() => {
    setShowLockScreen(false);
    setRestPaused(false);
    setRestPausedRemaining(null);
    setRestEndAt(null);
    if (restEnabled) {
      setNextRestAt(new Date(Date.now() + restMinutes * 60 * 1000));
    } else {
      setNextRestAt(null);
    }
    if (!isLockWindow) {
      invoke("set_gamma", {
        filterEnabled,
        strength: filterStrength,
        colorTemp,
      }).catch(() => undefined);
    }
  }, [
    restEnabled,
    restMinutes,
    isLockWindow,
    filterEnabled,
    filterStrength,
    colorTemp,
  ]);

  const handleTogglePause = useCallback(() => {
    if (!showLockScreen) return;
    if (restPaused) {
      if (restPausedRemaining === null) return;
      setRestEndAt(new Date(Date.now() + restPausedRemaining * 1000));
      setRestPaused(false);
      setRestPausedRemaining(null);
      return;
    }
    if (!restEndAt) return;
    const remaining = Math.max(
      0,
      Math.floor((restEndAt.getTime() - Date.now()) / 1000),
    );
    setRestPausedRemaining(remaining);
    setRestEndAt(null);
    setRestPaused(true);
  }, [restEndAt, restPaused, restPausedRemaining, showLockScreen]);

  const handleTogglePauseLocal = useCallback(() => {
    if (!isLockWindow) return;
    if (lockPausedLocal) {
      const nextEnd = Date.now() + lockRemainingLocal * 1000;
      setLockEndAtMs(nextEnd);
      setLockPausedLocal(false);
    } else {
      const remaining = lockEndAtMs
        ? Math.max(0, Math.floor((lockEndAtMs - Date.now()) / 1000))
        : 0;
      setLockRemainingLocal(remaining);
      setLockPausedLocal(true);
    }
  }, [isLockWindow, lockPausedLocal, lockRemainingLocal, lockEndAtMs]);

  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (isLockWindow) return;
    const reset = () => {
      invoke("reset_gamma").catch(() => undefined);
    };
    window.addEventListener("beforeunload", reset);
    return () => {
      window.removeEventListener("beforeunload", reset);
      reset();
    };
  }, [isLockWindow]);

  useEffect(() => {
    if (isLockWindow) return;
    let active = true;
    const handle = setTimeout(() => {
      invoke("set_gamma", {
        filterEnabled,
        strength: filterStrength,
        colorTemp,
      }).catch((error) => {
        if (active) {
          console.error("过滤蓝光设置失败", error);
        }
      });
    }, 80);
    return () => {
      active = false;
      clearTimeout(handle);
    };
  }, [isLockWindow, filterEnabled, filterStrength, colorTemp]);

  useEffect(() => {
    if (isLockWindow) return;
    invoke("prefetch_lock_wallpaper").catch((error) =>
      console.error("预取锁屏壁纸失败", error),
    );
  }, [isLockWindow]);

  useEffect(() => {
    if (isLockWindow) return;
    const oneDay = 24 * 60 * 60 * 1000;
    const timer = window.setInterval(() => {
      invoke("prefetch_lock_wallpaper").catch((error) =>
        console.error("预取锁屏壁纸失败", error),
      );
    }, oneDay);
    return () => window.clearInterval(timer);
  }, [isLockWindow]);

  useEffect(() => {
    if (isLockWindow) return;
    if (showLockScreen) {
      const endAt = restEndAt ?? new Date(Date.now() + restDuration * 60 * 1000);
      invoke("show_lock_windows", {
        endAtMs: endAt.getTime(),
        paused: restPaused,
        pausedRemaining: restPausedRemaining || 0,
        allowEsc: allowEscExit,
      }).catch((error) => console.error("锁屏窗口创建失败", error));
    } else {
      invoke("hide_lock_windows").catch((error) =>
        console.error("锁屏窗口关闭失败", error),
      );
    }
  }, [
    isLockWindow,
    showLockScreen,
    restEndAt,
    restDuration,
    restPaused,
    restPausedRemaining,
    allowEscExit,
  ]);

  useEffect(() => {
    if (isLockWindow) return;
    let unlisten: (() => void) | undefined;
    const window = getCurrentWebviewWindow();
    window
      .listen<string>("lockscreen-action", (event) => {
        if (event.payload === "exit") {
          handleExitRest();
        } else if (event.payload === "toggle_pause") {
          handleTogglePause();
        }
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((error) => console.error("监听锁屏动作失败", error));

    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [isLockWindow, handleExitRest, handleTogglePause]);

  useEffect(() => {
    if (!isLockWindow) return;
    const params = new URLSearchParams(window.location.search);
    const end = Number(params.get("end") || 0);
    const paused = params.get("paused") === "1";
    const remaining = Number(params.get("remaining") || 0);
    const allowEsc = params.get("allowEsc") !== "0";
    setLockEndAtMs(end > 0 ? end : null);
    setLockPausedLocal(paused);
    setLockRemainingLocal(remaining);
    setLockPayload((prev) => ({
      ...prev,
      allowEscExit: allowEsc,
    }));
  }, [isLockWindow]);

  useEffect(() => {
    if (!isLockWindow) return;
    let active = true;
    invoke<string | null>("get_lock_wallpaper")
      .then((path) => {
        if (!active) return;
        if (path) {
          const url = convertFileSrc(path);
          setLockBackgroundUrl(url);
          setLockWallpaperHistory([url]);
          setLockWallpaperIndex(0);
        } else {
          setLockBackgroundUrl(null);
          setLockWallpaperHistory([]);
          setLockWallpaperIndex(0);
        }
      })
      .catch((error) => {
        console.error("获取锁屏壁纸失败", error);
        setLockBackgroundUrl(null);
        setLockWallpaperHistory([]);
        setLockWallpaperIndex(0);
      });
    return () => {
      active = false;
    };
  }, [isLockWindow]);

  const handleNextWallpaper = useCallback(() => {
    if (!isLockWindow) return;
    if (lockWallpaperIndex < lockWallpaperHistory.length - 1) {
      const nextIndex = lockWallpaperIndex + 1;
      setLockWallpaperIndex(nextIndex);
      setLockBackgroundUrl(lockWallpaperHistory[nextIndex]);
      return;
    }
    invoke<string | null>("get_lock_wallpaper")
      .then((path) => {
        if (!path) return;
        const url = convertFileSrc(path);
        setLockWallpaperHistory((prev) => [...prev, url]);
        setLockWallpaperIndex((prev) => prev + 1);
        setLockBackgroundUrl(url);
      })
      .catch((error) => console.error("切换壁纸失败", error));
  }, [isLockWindow, lockWallpaperHistory, lockWallpaperIndex]);

  const handlePrevWallpaper = useCallback(() => {
    if (!isLockWindow) return;
    if (lockWallpaperIndex <= 0) return;
    const nextIndex = lockWallpaperIndex - 1;
    setLockWallpaperIndex(nextIndex);
    setLockBackgroundUrl(lockWallpaperHistory[nextIndex]);
  }, [isLockWindow, lockWallpaperHistory, lockWallpaperIndex]);

  useEffect(() => {
    if (!isLockWindow) return;
    const timer = setInterval(() => {
      const nowValue = new Date();
      const timeValue = nowValue.toLocaleTimeString("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
      });
      const dateValue = nowValue.toLocaleDateString("zh-CN", {
        month: "long",
        day: "numeric",
        weekday: "short",
      });

      let countdown = "00:00:00";
      if (lockPausedLocal) {
        countdown = formatDuration(lockRemainingLocal);
      } else if (lockEndAtMs) {
        countdown = formatDuration((lockEndAtMs - nowValue.getTime()) / 1000);
      }

      setLockPayload((prev) => ({
        ...prev,
        timeText: timeValue,
        dateText: dateValue,
        restCountdown: countdown,
        restPaused: lockPausedLocal,
      }));
    }, 500);
    return () => clearInterval(timer);
  }, [isLockWindow, lockEndAtMs, lockPausedLocal, lockRemainingLocal]);

  useEffect(() => {
    if (!isLockWindow) return;
    function onKeydown(event: KeyboardEvent) {
      if (!lockPayload.allowEscExit) return;
      if (event.key === "Escape") {
        invoke("lockscreen_action", { action: "exit" }).catch((error) =>
          console.error("锁屏退出失败", error),
        );
      }
    }
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  }, [isLockWindow, lockPayload.allowEscExit]);

  // 全局快捷键已取消

  useEffect(() => {
    if (showLockScreen) return;
    if (!restEnabled) {
      setNextRestAt(null);
      return;
    }
    const next = new Date(Date.now() + restMinutes * 60 * 1000);
    setNextRestAt(next);
  }, [showLockScreen, restEnabled, restMinutes]);

  useEffect(() => {
    if (!restEnabled || showLockScreen) return;
    if (!nextRestAt) return;
    if (now.getTime() >= nextRestAt.getTime()) {
      const endAt = new Date(Date.now() + restDuration * 60 * 1000);
      setRestPaused(false);
      setRestPausedRemaining(null);
      setRestEndAt(endAt);
      setShowLockScreen(true);
    }
  }, [now, restEnabled, nextRestAt, restDuration, showLockScreen]);

  useEffect(() => {
    if (!showLockScreen || !restEndAt) return;
    if (restPaused) return;
    if (now.getTime() >= restEndAt.getTime()) {
      handleExitRest();
    }
  }, [handleExitRest, now, restPaused, restEndAt, showLockScreen]);

  useEffect(() => {
    if (!showLockScreen) return;
    if (restPaused) {
      setRestPausedRemaining(restDuration * 60);
      return;
    }
    setRestEndAt(new Date(Date.now() + restDuration * 60 * 1000));
  }, [restDuration, showLockScreen, restPaused]);

  useEffect(() => {
    if (!showLockScreen) return;
    function onKeydown(event: KeyboardEvent) {
      if (!allowEscExit) return;
      if (event.key === "Escape") {
        handleExitRest();
      }
    }
    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  }, [showLockScreen, allowEscExit, handleExitRest]);

  useEffect(() => {
    if (showLockScreen) return;
    if (!restEnabled || !nextRestAt) return;
    if (now.getTime() < nextRestAt.getTime()) return;
    setNextRestAt(new Date(Date.now() + restMinutes * 60 * 1000));
  }, [now, showLockScreen, restEnabled, nextRestAt, restMinutes]);

  const nextRestCountdown = restEnabled && nextRestAt
    ? formatDuration((nextRestAt.getTime() - now.getTime()) / 1000)
    : "已暂停";

  const restCountdownSeconds =
    showLockScreen && restPaused && restPausedRemaining !== null
      ? restPausedRemaining
      : showLockScreen && restEndAt
        ? (restEndAt.getTime() - now.getTime()) / 1000
        : restDuration * 60;
  const restCountdown = formatDuration(restCountdownSeconds);

  const timeText = now.toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const dateText = now.toLocaleDateString("zh-CN", {
    month: "long",
    day: "numeric",
    weekday: "short",
  });
  const usageText = formatUsage((now.getTime() - sessionStart) / 1000);

  useEffect(() => {
    if (isLockWindow) return;
    if (!showLockScreen) return;
    invoke("broadcast_lock_update", {
      timeText,
      dateText,
      restCountdown,
      restPaused,
      allowEscExit,
    }).catch((error) => console.error("锁屏数据同步失败", error));
  }, [
    isLockWindow,
    showLockScreen,
    timeText,
    dateText,
    restCountdown,
    restPaused,
    allowEscExit,
  ]);


  useEffect(() => {
    if (isLockWindow) return;
    if (!showLockScreen) return;
    const timer = setInterval(() => {
      invoke("broadcast_lock_update", {
        timeText,
        dateText,
        restCountdown,
        restPaused,
        allowEscExit,
      }).catch((error) => console.error("锁屏数据同步失败", error));
    }, 1000);
    return () => clearInterval(timer);
  }, [
    isLockWindow,
    showLockScreen,
    timeText,
    dateText,
    restCountdown,
    restPaused,
    allowEscExit,
  ]);

  return (
    <div className="app">
      {!isLockWindow && (
        <>
          <div className="ambient ambient--one" />
          <div className="ambient ambient--two" />
          <div className="ambient ambient--grid" />

          <header className="topbar">
            <div className="brand">
              <span className="brand__dot" />
              <div>
                <p className="brand__name">护眼吧</p>
                <p className="brand__tag">清醒护眼 · 专注节奏</p>
              </div>
            </div>
            <div className="topbar__right">
              <div className="time-pill">
                <span>{timeText}</span>
                <span className="time-pill__date">{dateText}</span>
              </div>
            </div>
          </header>

          <section className="hero">
            <div className="hero__text">
              <p className="hero__kicker">今日护眼状态</p>
              <h1>保持专注，但别忘了松一口气。</h1>
              <p className="hero__subtitle">
                根据你的作息自动调节屏幕色温与休息节奏，让眼睛更舒适。
              </p>
              <div className="hero__stats">
                <div>
                  <p className="stat__label">连续使用</p>
                  <p className="stat__value">{usageText}</p>
                </div>
                <div>
                  <p className="stat__label">下一次休息</p>
                  <p className="stat__value">{nextRestCountdown}</p>
                </div>
              </div>
            </div>
            <div className="hero__panel">
              <div className="hero__orb" />
              <div className="hero__panel-inner">
                <p className="hero__panel-title">护眼模式已开启</p>
                <p className="hero__panel-desc">
                  当前为 <strong>{activePreset}</strong> 预设，过滤强度{" "}
                  <strong>{filterStrength}%</strong>。
                </p>
                <button className="btn btn--primary" type="button">
                  进入专注模式
                </button>
              </div>
            </div>
          </section>

          <section className="main-grid">
        <div className="card">
          <div className="card__header">
            <div>
              <p className="card__eyebrow">护眼滤镜</p>
              <h2>过滤蓝光</h2>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={filterEnabled}
                onChange={() => setFilterEnabled((prev) => !prev)}
              />
              <span className="toggle__track" />
            </label>
          </div>

          <div className="slider-group">
            <div className="slider-row">
              <span>强度</span>
              <span>{filterStrength}%</span>
            </div>
            <input
              type="range"
              min={0}
              max={100}
              value={filterStrength}
              onChange={(event) => setFilterStrength(Number(event.target.value))}
            />
          </div>

          <div className="chips">
            {(Object.keys(presets) as Array<keyof typeof presets>).map(
              (preset) => (
                <button
                  key={preset}
                  type="button"
                  className={`chip ${
                    activePreset === preset ? "chip--active" : ""
                  }`}
                  onClick={() => {
                    setActivePreset(preset);
                    const next = resolvePreset(preset);
                    setFilterStrength(next.strength);
                    setColorTemp(next.temp);
                    setFilterEnabled(true);
                  }}
                >
                  {preset}
                </button>
              ),
            )}
          </div>

          <div className="slider-group">
            <div className="slider-row">
              <span>色调</span>
              <span>{colorTemp}K</span>
            </div>
            <input
              type="range"
              min={2000}
              max={6500}
              step={100}
              value={colorTemp}
              onChange={(event) => setColorTemp(Number(event.target.value))}
            />
          </div>
        </div>

        <div className="card">
          <div className="card__header">
            <div>
              <p className="card__eyebrow">定时休息</p>
              <h2>休息节奏</h2>
            </div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={restEnabled}
                onChange={() => setRestEnabled((prev) => !prev)}
              />
              <span className="toggle__track" />
            </label>
          </div>

          <div className="pill-row">
            <div className="pill">
              <p className="pill__label">每隔</p>
              <input
                className="pill__input"
                type="number"
                min={15}
                max={120}
                value={restMinutes}
                onChange={(event) => setRestMinutes(Number(event.target.value))}
              />
              <span>分钟</span>
            </div>
            <div className="pill">
              <p className="pill__label">休息</p>
              <input
                className="pill__input"
                type="number"
                min={3}
                max={20}
                value={restDuration}
                onChange={(event) => setRestDuration(Number(event.target.value))}
              />
              <span>分钟</span>
            </div>
          </div>

          <div className="rest-countdown">
            <p>距离下次休息还有</p>
            <h3>{nextRestCountdown}</h3>
          </div>

          <button
            className="btn btn--ghost"
            type="button"
            onClick={handleStartRest}
          >
            立即进入休息
          </button>
        </div>

        <div className="card">
          <div className="card__header">
            <div>
              <p className="card__eyebrow">系统设置</p>
              <h2>快捷与托盘</h2>
            </div>
          </div>

          <div className="settings">
            <label className="setting-row">
              <span>锁屏允许 ESC 退出</span>
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={allowEscExit}
                  onChange={() => setAllowEscExit((prev) => !prev)}
                />
                <span className="toggle__track" />
              </label>
            </label>

            <label className="setting-row">
              <span>开机自启</span>
              <label className="toggle">
                <input type="checkbox" />
                <span className="toggle__track" />
              </label>
            </label>
          </div>
        </div>
          </section>

          <section className="preview-row">
        <div className="card card--preview">
          <div>
            <p className="card__eyebrow">锁屏预览</p>
            <h2>沉浸式休息</h2>
            <p className="helper-text">
              全屏遮罩不会真正锁定系统，倒计时结束或按 ESC 返回工作界面。
            </p>
          </div>
          <button
            className="btn btn--primary"
            type="button"
            onClick={handleStartRest}
          >
            打开预览
          </button>
        </div>

        <div className="card card--status">
          <p className="card__eyebrow">今日提示</p>
          <h3>休息 6 分钟即可恢复 30% 视觉疲劳</h3>
          <div className="status-list">
            <div>
              <p className="stat__label">过滤强度</p>
              <p className="stat__value">{filterStrength}%</p>
            </div>
            <div>
              <p className="stat__label">建议眨眼频率</p>
              <p className="stat__value">18 次/分钟</p>
            </div>
          </div>
        </div>
          </section>
        </>
      )}

      {isLockWindow && (
        <div
          className="lockscreen"
          style={
            lockBackgroundUrl
              ? { ["--lockscreen-bg" as string]: `url(${lockBackgroundUrl})` }
              : undefined
          }
        >
          <div className="lockscreen__scrim" />
          <div className="lockscreen__nav">
            <button
              className="lockscreen__nav-btn"
              type="button"
              onClick={handlePrevWallpaper}
              aria-label="上一张壁纸"
            >
              {"<"}
            </button>
            <button
              className="lockscreen__nav-btn"
              type="button"
              onClick={handleNextWallpaper}
              aria-label="下一张壁纸"
            >
              {">"}
            </button>
          </div>
          <div className="lockscreen__content">
            <div className="lockscreen__top">
              <div>
                <p className="lockscreen__time">{lockPayload.timeText}</p>
                <p className="lockscreen__date">{lockPayload.dateText}</p>
              </div>
              <div />
            </div>
            <div className="lockscreen__center">
              <p>休息一下，放松眼睛</p>
              <div className="lockscreen__timer">
                <p className="lockscreen__timer-label">剩余时间</p>
                <div
                  className={`lockscreen__timer-value ${
                    lockPayload.restPaused ? "is-paused" : ""
                  }`}
                >
                  {lockPayload.restCountdown.replaceAll(":", " : ")}
                </div>
                <p className="lockscreen__timer-hint">
                  {lockPayload.restPaused
                    ? "计时已暂停，点击继续恢复倒计时"
                    : "闭眼 20 秒，眺望远处 20 秒"}
                </p>
              </div>
              <p className="lockscreen__quote">
                “短暂离开屏幕，给眼睛一次深呼吸。”
              </p>
            </div>
            <div className="lockscreen__actions">
              {lockPayload.allowEscExit ? (
                <span className="helper-text">ESC 退出已开启</span>
              ) : (
                <span className="helper-text">ESC 已禁用</span>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
