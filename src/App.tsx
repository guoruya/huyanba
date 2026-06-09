import {
  type FormEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
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

type WallpaperRefreshResult = {
  sourceKind: string;
  sourceLabel: string;
  listSuccesses: number;
  added: number;
};

type AppView = "dashboard" | "wallpapers";

type RemoteWallpaperSource = "unsplash" | "palace";

type UnsplashDownloadPayload = {
  id: string;
  rawUrl: string;
  downloadLocation: string;
  thumbUrl: string;
  photoUrl: string;
  authorName: string;
  authorUrl: string;
};

type PalaceStagingWallpaperSummary = {
  id: string;
  title: string;
  path: string;
  sourceUrl: string;
  width: number;
  height: number;
  creditName: string;
  creditUrl: string;
  photoUrl: string;
  addedAt: number;
};

type PalaceStagingBatchResult = {
  fetched: number;
  replacedPreviousBatch: boolean;
  page: number;
  hasPrevPage: boolean;
  hasNextPage: boolean;
  maxPage: number;
  processedCount: number;
  skippedCount: number;
  remainingItems: number;
  items: PalaceStagingWallpaperSummary[];
};

type PalaceStagingRefreshState =
  | "idle"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

type PalaceStagingRefreshStatus = {
  state: PalaceStagingRefreshState;
  targetPage: number;
  currentCommittedPage: number;
  processedEntries: number;
  totalEntries: number;
  message: string;
  errorMessage?: string | null;
  batch?: PalaceStagingBatchResult | null;
};

type RemoteWallpaperSummary = {
  source: RemoteWallpaperSource;
  id: string;
  title: string;
  description: string;
  width: number;
  height: number;
  thumbUrl: string;
  previewUrl: string;
  creditName: string;
  creditUrl: string;
  photoUrl: string;
  downloadPayload: UnsplashDownloadPayload;
};

type RemoteWallpaperSearchResult = {
  source: RemoteWallpaperSource;
  configured: boolean;
  page: number;
  perPage: number;
  totalPages: number;
  totalResults: number;
  hasNextPage: boolean;
  items: RemoteWallpaperSummary[];
  errorMessage?: string | null;
};

type LocalWallpaperSummary = {
  path: string;
  sourceUrl: string;
  sourceKind: string;
  addedAt: number;
  lastShownAt: number;
  thumbUrl: string;
  authorName: string;
  authorUrl: string;
  photoUrl: string;
  isFixed: boolean;
};

type DownloadWallpaperResult = {
  added: boolean;
  sourceUrl: string;
  path: string;
  isFixed: boolean;
  wallpaper: LocalWallpaperSummary;
};

type WallpaperStorageSettings = {
  currentDir: string;
  defaultDir: string;
  isDefault: boolean;
};

type WallpaperStorageUpdateResult = {
  settings: WallpaperStorageSettings;
  migratedFiles: number;
  restoredDefault: boolean;
};

type UnsplashConfigSource = "appConfig" | "envLocal" | "env" | "builtIn" | "none";

type UnsplashSettings = {
  effectiveConfigured: boolean;
  configSource: UnsplashConfigSource;
  hasStoredKey: boolean;
  maskedStoredKey?: string | null;
  isBuiltIn: boolean;
};

const WALLPAPER_PAGE_SIZE = 12;

function describeWallpaper(item: {
  title?: string;
  description?: string;
}) {
  return item.title || item.description || "未命名壁纸";
}

function getRemoteSourceLabel(source: RemoteWallpaperSource) {
  return source === "unsplash" ? "Unsplash" : "故宫壁纸";
}

function getLocalSourceLabel(sourceKind: string) {
  if (sourceKind === "unsplash") return "Unsplash";
  if (sourceKind === "palace") return "故宫壁纸";
  if (sourceKind === "local") return "本地恢复";
  return "旧缓存";
}

function formatAddedAt(timestamp: number) {
  if (!timestamp) return "刚刚加入";
  return new Date(timestamp * 1000).toLocaleString("zh-CN", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function extractErrorMessage(error: unknown) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "toString" in error) {
    return String(error);
  }
  return "未知错误";
}

function describeWallpaperError(error: unknown) {
  const message = extractErrorMessage(error);
  if (message.includes("UNSPLASH_ACCESS_KEY")) {
    return "还没有配置 Unsplash Access Key，先在壁纸设置里填写，或继续使用环境变量。";
  }
  if (message.includes("故宫候选壁纸获取失败")) {
    return "故宫壁纸这次没有成功抓到候选图片，稍后再试一次。";
  }
  if (message.includes("未找到指定故宫候选壁纸")) {
    return "这张故宫候选壁纸已经失效，请重新获取一批。";
  }
  if (message.includes("故宫候选图片文件不存在")) {
    return "这张故宫候选图片已经不存在了，请重新获取一批。";
  }
  if (message.includes("故宫壁纸请求失败")) {
    return "故宫壁纸源当前没有返回可用数据，稍后再试一次。";
  }
  if (message.includes("429")) {
    return "Unsplash 请求过于频繁，稍后再试。";
  }
  if (message.includes("403") || message.includes("401")) {
    return "Unsplash Access Key 无效或没有权限。";
  }
  if (message.includes("目标目录必须为空")) {
    return "目标目录里已经有文件了，请换一个空文件夹再保存。";
  }
  if (message.includes("请输入绝对路径")) {
    return "请输入完整的绝对路径，或直接使用“选择文件夹”按钮。";
  }
  if (message.includes("不可创建或不可写")) {
    return "这个目录当前不可创建或不可写，请检查路径和权限后再试。";
  }
  if (message.includes("恢复默认失败")) {
    return "恢复默认目录失败，现有壁纸仍保留在旧目录里。";
  }
  if (message.includes("迁移壁纸失败")) {
    return "迁移壁纸失败，这次没有切换目录，请稍后重试。";
  }
  return message;
}

function describeUnsplashConfigSource(source: UnsplashConfigSource) {
  if (source === "appConfig") return "应用内配置";
  if (source === "envLocal") return ".env.local";
  if (source === "env") return "环境变量";
  if (source === "builtIn") return "内置 Key";
  return "未配置";
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
  const [lockSessionFixedMode, setLockSessionFixedMode] = useState(false);
  const [wallpaperRefreshPending, setWallpaperRefreshPending] = useState(false);
  const [activeView, setActiveView] = useState<AppView>("dashboard");
  const [activeRemoteSource, setActiveRemoteSource] =
    useState<RemoteWallpaperSource>("unsplash");
  const [unsplashSearchInput, setUnsplashSearchInput] = useState("");
  const [unsplashSearchQuery, setUnsplashSearchQuery] = useState("");
  const [unsplashSearchResult, setUnsplashSearchResult] =
    useState<RemoteWallpaperSearchResult | null>(null);
  const [unsplashSearchLoading, setUnsplashSearchLoading] = useState(false);
  const [unsplashSearchError, setUnsplashSearchError] = useState<string | null>(
    null,
  );
  const [selectedUnsplashWallpaperId, setSelectedUnsplashWallpaperId] = useState<
    string | null
  >(null);
  const [palaceStagingWallpapers, setPalaceStagingWallpapers] = useState<
    PalaceStagingWallpaperSummary[]
  >([]);
  const [palaceStagingLoading, setPalaceStagingLoading] = useState(false);
  const [palaceStagingError, setPalaceStagingError] = useState<string | null>(
    null,
  );
  const [palaceStagingBootstrapped, setPalaceStagingBootstrapped] =
    useState(false);
  const [palaceStagingPage, setPalaceStagingPage] = useState(1);
  const [palaceStagingHasPrevPage, setPalaceStagingHasPrevPage] = useState(false);
  const [palaceStagingHasNextPage, setPalaceStagingHasNextPage] = useState(false);
  const [palaceStagingMaxPage, setPalaceStagingMaxPage] = useState(1);
  const [palaceRefreshStatus, setPalaceRefreshStatus] =
    useState<PalaceStagingRefreshStatus | null>(null);
  const [selectedPalaceStagingSource, setSelectedPalaceStagingSource] =
    useState<string | null>(null);
  const [palaceBatchMode, setPalaceBatchMode] = useState(false);
  const [selectedPalaceStageSources, setSelectedPalaceStageSources] = useState<
    string[]
  >([]);
  const [localWallpapers, setLocalWallpapers] = useState<LocalWallpaperSummary[]>(
    [],
  );
  const [localWallpapersLoading, setLocalWallpapersLoading] = useState(false);
  const [localWallpapersError, setLocalWallpapersError] = useState<string | null>(
    null,
  );
  const [selectedLocalWallpaperSource, setSelectedLocalWallpaperSource] =
    useState<string | null>(null);
  const [wallpaperStorageSettings, setWallpaperStorageSettings] =
    useState<WallpaperStorageSettings | null>(null);
  const [unsplashSettings, setUnsplashSettings] =
    useState<UnsplashSettings | null>(null);
  const [wallpaperSettingsOpen, setWallpaperSettingsOpen] = useState(false);
  const [wallpaperStorageInput, setWallpaperStorageInput] = useState("");
  const [unsplashAccessKeyInput, setUnsplashAccessKeyInput] = useState("");
  const [wallpaperStoragePending, setWallpaperStoragePending] = useState(false);
  const [unsplashSettingsPending, setUnsplashSettingsPending] = useState(false);
  const [wallpaperStorageTone, setWallpaperStorageTone] = useState<
    "muted" | "success" | "error"
  >("muted");
  const [wallpaperStorageMessage, setWallpaperStorageMessage] = useState(
    "默认仍然保存到应用缓存目录，也可以切到你指定的空文件夹。",
  );
  const [unsplashSettingsTone, setUnsplashSettingsTone] = useState<
    "muted" | "success" | "error"
  >("muted");
  const [unsplashSettingsMessage, setUnsplashSettingsMessage] = useState(
    "这里只需要填写 Unsplash Access Key，不需要 Application ID 或 Secret Key。",
  );
  const [wallpaperActionPending, setWallpaperActionPending] = useState<
    | ""
    | "download"
    | "fixed"
    | "promote"
    | "promote-fixed"
    | "promote-bulk"
    | "fixed-local"
    | "clear-fixed"
    | "delete"
  >("");
  const [wallpaperActionTone, setWallpaperActionTone] = useState<
    "muted" | "success" | "error"
  >("muted");
  const [wallpaperActionMessage, setWallpaperActionMessage] = useState(
    "Unsplash 可以直接下载到本地；故宫会先展示候选批次，再决定是否加入壁纸库。",
  );
  const exitInProgressRef = useRef(false);
  const exitRestRef = useRef<() => void>(() => {});
  const togglePauseRef = useRef<() => void>(() => {});
  const palaceRefreshIntentRef = useRef<"auto" | "manual" | "page" | null>(null);

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
    exitInProgressRef.current = false;
    const endAt = new Date(Date.now() + restDuration * 60 * 1000);
    setRestPaused(false);
    setRestPausedRemaining(null);
    setRestEndAt(endAt);
    setShowLockScreen(true);
  }, [restDuration]);

  const selectedUnsplashWallpaper = useMemo(
    () =>
      unsplashSearchResult?.items.find(
        (item) => item.id === selectedUnsplashWallpaperId,
      ) ??
      unsplashSearchResult?.items[0] ??
      null,
    [selectedUnsplashWallpaperId, unsplashSearchResult],
  );
  const selectedPalaceStagingWallpaper = useMemo(
    () =>
      palaceStagingWallpapers.find(
        (item) => item.sourceUrl === selectedPalaceStagingSource,
      ) ??
      palaceStagingWallpapers[0] ??
      null,
    [palaceStagingWallpapers, selectedPalaceStagingSource],
  );

  const selectedPalaceStageCount = selectedPalaceStageSources.length;

  const selectedLocalWallpaper = useMemo(
    () =>
      localWallpapers.find(
        (item) => item.sourceUrl === selectedLocalWallpaperSource,
      ) ??
      localWallpapers[0] ??
      null,
    [localWallpapers, selectedLocalWallpaperSource],
  );

  const loadWallpaperStorageSettings = useCallback(async () => {
    if (isLockWindow) return;
    try {
      const settings = await invoke<WallpaperStorageSettings>(
        "get_wallpaper_storage_settings",
      );
      setWallpaperStorageSettings(settings);
      setWallpaperStorageInput(settings.currentDir);
    } catch (error) {
      console.error("加载壁纸目录设置失败", error);
      setWallpaperStorageTone("error");
      setWallpaperStorageMessage(describeWallpaperError(error));
    }
  }, [isLockWindow]);

  const loadUnsplashSettings = useCallback(async () => {
    if (isLockWindow) return null;
    try {
      const settings = await invoke<UnsplashSettings>("get_unsplash_settings");
      setUnsplashSettings(settings);
      return settings;
    } catch (error) {
      console.error("加载 Unsplash 配置失败", error);
      setUnsplashSettingsTone("error");
      setUnsplashSettingsMessage(describeWallpaperError(error));
      return null;
    }
  }, [isLockWindow]);

  const loadLocalWallpapers = useCallback(async () => {
    if (isLockWindow) return;
    setLocalWallpapersLoading(true);
    setLocalWallpapersError(null);
    try {
      const items = await invoke<LocalWallpaperSummary[]>("list_local_wallpapers");
      setLocalWallpapers(items);
      setSelectedLocalWallpaperSource((prev) =>
        items.some((item) => item.sourceUrl === prev)
          ? prev
          : items[0]?.sourceUrl ?? null,
      );
      return items;
    } catch (error) {
      console.error("加载本地壁纸失败", error);
      setLocalWallpapersError(describeWallpaperError(error));
      return [];
    } finally {
      setLocalWallpapersLoading(false);
    }
  }, [isLockWindow]);

  const reloadLockWallpaperFromStorage = useCallback(async () => {
    const resolvedPath = await invoke<string | null>("get_lock_wallpaper");
    if (resolvedPath) {
      const url = convertFileSrc(resolvedPath);
      let history = [url];
      let fixedMode = false;
      try {
        const items = await invoke<LocalWallpaperSummary[]>("list_local_wallpapers");
        const fixedItem = items.find((item) => item.isFixed);
        if (fixedItem && fixedItem.path === resolvedPath) {
          history = [
            url,
            ...items
              .filter((item) => item.path !== resolvedPath)
              .map((item) => convertFileSrc(item.path)),
          ];
          fixedMode = true;
        }
      } catch (error) {
        console.error("加载锁屏会话壁纸列表失败", error);
      }
      setLockSessionFixedMode(fixedMode);
      setLockBackgroundUrl(url);
      setLockWallpaperHistory(history);
      setLockWallpaperIndex(0);
    } else {
      setLockSessionFixedMode(false);
      setLockBackgroundUrl(null);
      setLockWallpaperHistory([]);
      setLockWallpaperIndex(0);
    }
  }, []);

  const runUnsplashSearch = useCallback(
    async (query: string, page: number) => {
      if (isLockWindow) return;
      setUnsplashSearchLoading(true);
      setUnsplashSearchError(null);
      try {
        const result = await invoke<RemoteWallpaperSearchResult>(
          "search_unsplash_wallpapers",
          {
            query,
            page,
            perPage: WALLPAPER_PAGE_SIZE,
          },
        );
        setUnsplashSearchQuery(query);
        setUnsplashSearchResult(result);
        setSelectedUnsplashWallpaperId((prev) =>
          result.items.some((item) => item.id === prev)
            ? prev
            : result.items[0]?.id ?? null,
        );
        if (!result.configured) {
          setActiveRemoteSource((prev) =>
            prev === "unsplash" ? "palace" : prev,
          );
        }
      } catch (error) {
        console.error("搜索 Unsplash 壁纸失败", error);
        setUnsplashSearchError(describeWallpaperError(error));
      } finally {
        setUnsplashSearchLoading(false);
      }
    },
    [isLockWindow],
  );

  const applyPalaceStagingResult = useCallback(
    (result: PalaceStagingBatchResult) => {
      setPalaceStagingWallpapers(result.items);
      setPalaceStagingPage(result.page || 1);
      setPalaceStagingHasPrevPage(result.hasPrevPage);
      setPalaceStagingHasNextPage(result.hasNextPage);
      setPalaceStagingMaxPage(result.maxPage || result.page || 1);
      setSelectedPalaceStageSources((prev) =>
        prev.filter((sourceUrl) =>
          result.items.some((item) => item.sourceUrl === sourceUrl),
        ),
      );
      setSelectedPalaceStagingSource((prev) =>
        result.items.some((item) => item.sourceUrl === prev)
          ? prev
          : result.items[0]?.sourceUrl ?? null,
      );
      return result;
    },
    [],
  );

  const applyPalaceRefreshStatus = useCallback(
    (status: PalaceStagingRefreshStatus) => {
      setPalaceRefreshStatus(status);

      if (status.state === "running") {
        setPalaceStagingError(null);
        return status;
      }

      if (status.state === "succeeded") {
        if (status.batch) {
          applyPalaceStagingResult(status.batch);
        }
        setPalaceBatchMode(false);
        setSelectedPalaceStageSources([]);
        setPalaceStagingBootstrapped(true);
        setPalaceStagingError(null);
        if (
          palaceRefreshIntentRef.current === "manual" ||
          palaceRefreshIntentRef.current === "page"
        ) {
          setWallpaperActionTone("success");
          setWallpaperActionMessage(
            status.batch && status.batch.items.length > 0
              ? `已获取故宫第 ${status.batch.page} 页，当前有 ${status.batch.items.length} 张候选图。`
              : `故宫第 ${status.targetPage} 页当前没有可预览的候选壁纸。`,
          );
        }
        palaceRefreshIntentRef.current = null;
        return status;
      }

      if (status.state === "failed") {
        const message =
          palaceStagingWallpapers.length > 0
            ? "这次刷新失败，但仍保留当前页候选壁纸。"
            : describeWallpaperError(
                status.errorMessage ?? "故宫候选壁纸获取失败。",
              );
        setPalaceStagingError(message);
        if (
          palaceRefreshIntentRef.current === "manual" ||
          palaceRefreshIntentRef.current === "page"
        ) {
          setWallpaperActionTone("error");
          setWallpaperActionMessage(message);
        }
        palaceRefreshIntentRef.current = null;
      }

      if (status.state === "cancelled") {
        setPalaceStagingError(null);
        setWallpaperActionTone("muted");
        setWallpaperActionMessage(
          status.message || "故宫候选壁纸获取已取消。",
        );
        palaceRefreshIntentRef.current = null;
      }

      return status;
    },
    [applyPalaceStagingResult, palaceStagingWallpapers.length],
  );

  const loadPalaceStagingWallpapers = useCallback(async () => {
    if (isLockWindow) {
      return {
        fetched: 0,
        replacedPreviousBatch: false,
        page: 1,
        hasPrevPage: false,
        hasNextPage: false,
        maxPage: 1,
        processedCount: 0,
        skippedCount: 0,
        remainingItems: 0,
        items: [],
      } satisfies PalaceStagingBatchResult;
    }
    setPalaceStagingLoading(true);
    setPalaceStagingError(null);
    try {
      const result = await invoke<PalaceStagingBatchResult>(
        "get_palace_staging_batch",
      );
      return applyPalaceStagingResult(result);
    } catch (error) {
      console.error("加载故宫候选壁纸失败", error);
      setPalaceStagingError(describeWallpaperError(error));
      return {
        fetched: 0,
        replacedPreviousBatch: false,
        page: palaceStagingPage,
        hasPrevPage: palaceStagingHasPrevPage,
        hasNextPage: palaceStagingHasNextPage,
        maxPage: palaceStagingMaxPage,
        processedCount: 0,
        skippedCount: 0,
        remainingItems: palaceStagingWallpapers.length,
        items: palaceStagingWallpapers,
      } satisfies PalaceStagingBatchResult;
    } finally {
      setPalaceStagingLoading(false);
    }
  }, [
    applyPalaceStagingResult,
    isLockWindow,
    palaceStagingHasNextPage,
    palaceStagingHasPrevPage,
    palaceStagingMaxPage,
    palaceStagingPage,
    palaceStagingWallpapers,
  ]);

  const loadPalaceRefreshStatus = useCallback(async () => {
    if (isLockWindow) {
      const status = {
        state: "idle",
        targetPage: 1,
        currentCommittedPage: 1,
        processedEntries: 0,
        totalEntries: 0,
        message: "",
        errorMessage: null,
        batch: null,
      } satisfies PalaceStagingRefreshStatus;
      return applyPalaceRefreshStatus(status);
    }
    try {
      const status = await invoke<PalaceStagingRefreshStatus>(
        "get_palace_staging_refresh_status",
      );
      return applyPalaceRefreshStatus(status);
    } catch (error) {
      console.error("读取故宫后台刷新状态失败", error);
      const fallback = {
        state: "failed",
        targetPage: palaceStagingPage,
        currentCommittedPage: palaceStagingPage,
        processedEntries: 0,
        totalEntries: 0,
        message: "故宫后台刷新状态读取失败。",
        errorMessage: describeWallpaperError(error),
        batch: null,
      } satisfies PalaceStagingRefreshStatus;
      return applyPalaceRefreshStatus(fallback);
    }
  }, [applyPalaceRefreshStatus, isLockWindow, palaceStagingPage]);

  const refreshPalaceStagingBatch = useCallback(
    async (
      reason: "auto" | "manual" | "page" = "manual",
      page?: number,
    ) => {
      if (isLockWindow) {
        return {
          state: "idle",
          targetPage: 1,
          currentCommittedPage: 1,
          processedEntries: 0,
          totalEntries: 0,
          message: "",
          errorMessage: null,
          batch: null,
        } satisfies PalaceStagingRefreshStatus;
      }
      const targetPage = Math.max(1, page ?? palaceStagingPage ?? 1);
      palaceRefreshIntentRef.current = reason;
      setPalaceStagingError(null);
      if (reason === "manual" || reason === "page") {
        setWallpaperActionTone("muted");
        setWallpaperActionMessage(`正在后台获取第 ${targetPage} 页故宫候选壁纸。`);
      }
      try {
        const status = await invoke<PalaceStagingRefreshStatus>(
          "start_palace_staging_refresh",
          { page: targetPage },
        );
        return applyPalaceRefreshStatus(status);
      } catch (error) {
        console.error("启动故宫候选后台刷新失败", error);
        const fallback = applyPalaceRefreshStatus({
          state: "failed",
          targetPage,
          currentCommittedPage: palaceStagingPage,
          processedEntries: 0,
          totalEntries: 0,
          message: `故宫第 ${targetPage} 页候选壁纸获取失败。`,
          errorMessage: describeWallpaperError(error),
          batch: null,
        });
        if (reason === "manual" || reason === "page") {
          setWallpaperActionTone("error");
          setWallpaperActionMessage(describeWallpaperError(error));
        }
        return fallback;
      }
    },
    [
      applyPalaceRefreshStatus,
      isLockWindow,
      palaceStagingPage,
    ],
  );

  const handleRefreshWallpaper = useCallback(async () => {
    if (isLockWindow || wallpaperRefreshPending) return;
    setWallpaperRefreshPending(true);
    try {
      const result = await invoke<WallpaperRefreshResult>(
        "refresh_lock_wallpaper_now",
      );
      if (result.listSuccesses === 0) {
        return;
      }
      if (result.sourceKind === "palace") {
        setActiveView("wallpapers");
        setActiveRemoteSource("palace");
        setPalaceStagingBootstrapped(true);
        await loadPalaceStagingWallpapers();
        return;
      }
      void loadLocalWallpapers();
    } catch (error) {
      console.error("立即刷新壁纸失败", error);
    } finally {
      setWallpaperRefreshPending(false);
    }
  }, [
    isLockWindow,
    loadLocalWallpapers,
    loadPalaceStagingWallpapers,
    unsplashSearchResult,
    wallpaperRefreshPending,
  ]);

  const handleCancelPalaceRefresh = useCallback(async () => {
    if (isLockWindow) return;
    try {
      await invoke<PalaceStagingRefreshStatus>(
        "cancel_palace_staging_refresh",
      );
    } catch (error) {
      console.error("取消故宫候选壁纸获取失败", error);
    }
  }, [isLockWindow]);

  const handleUnsplashSearchSubmit = useCallback(
    async (event?: FormEvent<HTMLFormElement>) => {
      event?.preventDefault();
      await runUnsplashSearch(unsplashSearchInput, 1);
    },
    [runUnsplashSearch, unsplashSearchInput],
  );

  const handlePickWallpaperFolder = useCallback(async () => {
    if (palaceRefreshStatus?.state === "running") return;
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "选择壁纸保存目录",
      });
      if (typeof selected === "string") {
        setWallpaperStorageInput(selected);
        setWallpaperStorageTone("muted");
        setWallpaperStorageMessage("已选中文件夹，确认后会自动迁移现有壁纸。");
      }
    } catch (error) {
      console.error("选择壁纸目录失败", error);
      setWallpaperStorageTone("error");
      setWallpaperStorageMessage(describeWallpaperError(error));
    }
  }, [palaceRefreshStatus]);

  const handleSaveWallpaperStorageDir = useCallback(async () => {
    if (
      isLockWindow ||
      wallpaperStoragePending ||
      palaceRefreshStatus?.state === "running"
    ) {
      return;
    }
    const nextPath = wallpaperStorageInput.trim();
    if (!nextPath) {
      setWallpaperStorageTone("error");
      setWallpaperStorageMessage("请输入一个绝对路径，或直接点“恢复默认”。");
      return;
    }
    setWallpaperStoragePending(true);
    setWallpaperStorageTone("muted");
    setWallpaperStorageMessage("正在切换目录并迁移现有壁纸。");
    try {
      const result = await invoke<WallpaperStorageUpdateResult>(
        "set_wallpaper_storage_dir",
        { path: nextPath },
      );
      setWallpaperStorageSettings(result.settings);
      setWallpaperStorageInput(result.settings.currentDir);
      setWallpaperStorageTone("success");
      setWallpaperStorageMessage(
        result.migratedFiles > 0
          ? `已切换到新目录，并迁移了 ${result.migratedFiles} 个文件。`
          : "保存目录已更新，当前没有需要迁移的壁纸文件。",
      );
      await loadWallpaperStorageSettings();
      await loadLocalWallpapers();
    } catch (error) {
      console.error("设置壁纸目录失败", error);
      setWallpaperStorageTone("error");
      setWallpaperStorageMessage(describeWallpaperError(error));
    } finally {
      setWallpaperStoragePending(false);
    }
  }, [
    isLockWindow,
    loadLocalWallpapers,
    loadWallpaperStorageSettings,
    wallpaperStorageInput,
    wallpaperStoragePending,
    palaceRefreshStatus,
  ]);

  const handleResetWallpaperStorageDir = useCallback(async () => {
    if (
      isLockWindow ||
      wallpaperStoragePending ||
      palaceRefreshStatus?.state === "running"
    ) {
      return;
    }
    setWallpaperStoragePending(true);
    setWallpaperStorageTone("muted");
    setWallpaperStorageMessage("正在恢复默认目录并迁移现有壁纸。");
    try {
      const result = await invoke<WallpaperStorageUpdateResult>(
        "set_wallpaper_storage_dir",
        { path: null },
      );
      setWallpaperStorageSettings(result.settings);
      setWallpaperStorageInput(result.settings.currentDir);
      setWallpaperStorageTone("success");
      setWallpaperStorageMessage(
        result.migratedFiles > 0
          ? `已恢复默认目录，并迁移了 ${result.migratedFiles} 个文件。`
          : "已恢复默认目录。",
      );
      await loadWallpaperStorageSettings();
      await loadLocalWallpapers();
    } catch (error) {
      console.error("恢复默认壁纸目录失败", error);
      setWallpaperStorageTone("error");
      setWallpaperStorageMessage(describeWallpaperError(error));
    } finally {
      setWallpaperStoragePending(false);
    }
  }, [
    isLockWindow,
    loadLocalWallpapers,
    loadWallpaperStorageSettings,
    palaceRefreshStatus,
    wallpaperStoragePending,
  ]);

  const handleSaveUnsplashAccessKey = useCallback(async () => {
    if (
      isLockWindow ||
      unsplashSettingsPending ||
      palaceRefreshStatus?.state === "running"
    ) {
      return;
    }
    const accessKey = unsplashAccessKeyInput.trim();
    if (!accessKey) {
      setUnsplashSettingsTone("error");
      setUnsplashSettingsMessage("请输入 Unsplash Access Key。");
      return;
    }
    setUnsplashSettingsPending(true);
    setUnsplashSettingsTone("muted");
    setUnsplashSettingsMessage("正在保存 Unsplash Access Key。");
    try {
      const settings = await invoke<UnsplashSettings>("set_unsplash_access_key", {
        accessKey,
      });
      setUnsplashSettings(settings);
      setUnsplashAccessKeyInput("");
      setUnsplashSettingsTone("success");
      setUnsplashSettingsMessage("Unsplash Access Key 已保存，现在可以直接搜索和下载壁纸。");
      if (activeView === "wallpapers") {
        void runUnsplashSearch(unsplashSearchInput, 1);
      }
    } catch (error) {
      console.error("保存 Unsplash Access Key 失败", error);
      setUnsplashSettingsTone("error");
      setUnsplashSettingsMessage(describeWallpaperError(error));
    } finally {
      setUnsplashSettingsPending(false);
    }
  }, [
    activeView,
    isLockWindow,
    palaceRefreshStatus,
    runUnsplashSearch,
    unsplashAccessKeyInput,
    unsplashSearchInput,
    unsplashSettingsPending,
  ]);

  const handleClearUnsplashAccessKey = useCallback(async () => {
    if (
      isLockWindow ||
      unsplashSettingsPending ||
      palaceRefreshStatus?.state === "running"
    ) {
      return;
    }
    setUnsplashSettingsPending(true);
    setUnsplashSettingsTone("muted");
    setUnsplashSettingsMessage("正在清除应用内保存的 Unsplash Access Key。");
    try {
      const settings = await invoke<UnsplashSettings>("clear_unsplash_access_key");
      setUnsplashSettings(settings);
      setUnsplashAccessKeyInput("");
      setUnsplashSettingsTone("success");
      if (settings.effectiveConfigured) {
        setUnsplashSettingsMessage(
          `已清除应用内 Access Key，当前继续使用 ${describeUnsplashConfigSource(settings.configSource)}。`,
        );
        if (activeView === "wallpapers") {
          void runUnsplashSearch(unsplashSearchInput, 1);
        }
      } else {
        setUnsplashSettingsMessage("已清除应用内 Access Key，当前会继续使用故宫来源。");
        if (activeRemoteSource === "unsplash") {
          setActiveRemoteSource("palace");
        }
      }
    } catch (error) {
      console.error("清除 Unsplash Access Key 失败", error);
      setUnsplashSettingsTone("error");
      setUnsplashSettingsMessage(describeWallpaperError(error));
    } finally {
      setUnsplashSettingsPending(false);
    }
  }, [
    activeRemoteSource,
    activeView,
    isLockWindow,
    palaceRefreshStatus,
    runUnsplashSearch,
    unsplashSearchInput,
    unsplashSettingsPending,
  ]);

  const handleDownloadWallpaper = useCallback(
    async (setFixed: boolean) => {
      if (!selectedUnsplashWallpaper) return;
      const sourceLabel = getRemoteSourceLabel(selectedUnsplashWallpaper.source);
      setWallpaperActionPending(setFixed ? "fixed" : "download");
      setWallpaperActionTone("muted");
      setWallpaperActionMessage(`正在下载 ${sourceLabel} 壁纸，这可能需要几秒钟。`);
      try {
        const result = await invoke<DownloadWallpaperResult>(
          "download_unsplash_wallpaper",
          {
            payload: selectedUnsplashWallpaper.downloadPayload,
            setFixed,
          },
        );
        setWallpaperActionTone("success");
        if (setFixed) {
          setWallpaperActionMessage(`已从 ${sourceLabel} 下载并固定为当前锁屏壁纸。`);
        } else if (result.added) {
          setWallpaperActionMessage(`已从 ${sourceLabel} 下载到本地壁纸库。`);
        } else {
          setWallpaperActionMessage(`这张 ${sourceLabel} 壁纸已经在本地，信息已同步。`);
        }
        await loadLocalWallpapers();
        setSelectedLocalWallpaperSource(result.sourceUrl);
      } catch (error) {
        console.error("下载在线壁纸失败", error);
        setWallpaperActionTone("error");
        setWallpaperActionMessage(describeWallpaperError(error));
      } finally {
        setWallpaperActionPending("");
      }
    },
    [loadLocalWallpapers, selectedUnsplashWallpaper],
  );

  const handlePromotePalaceWallpaper = useCallback(
    async (setFixed: boolean) => {
      if (palaceRefreshStatus?.state === "running") return;
      if (!selectedPalaceStagingWallpaper) return;
      setWallpaperActionPending(setFixed ? "promote-fixed" : "promote");
      setWallpaperActionTone("muted");
      setWallpaperActionMessage(
        setFixed
          ? "正在把这张故宫候选图加入本地库并固定。"
          : "正在把这张故宫候选图加入本地库。",
      );
      try {
        const result = await invoke<DownloadWallpaperResult>(
          "promote_palace_staging_wallpaper",
          {
            sourceUrl: selectedPalaceStagingWallpaper.sourceUrl,
            setFixed,
          },
        );
        setWallpaperActionTone("success");
        setWallpaperActionMessage(
          setFixed
            ? "这张故宫壁纸已经加入本地库，并固定为当前锁屏壁纸。"
            : result.added
              ? "这张故宫壁纸已经加入本地壁纸库。"
              : "这张故宫壁纸原本就在本地库里，信息已同步。",
        );
        await Promise.all([
          loadLocalWallpapers(),
          loadPalaceStagingWallpapers(),
        ]);
        setSelectedLocalWallpaperSource(result.sourceUrl);
      } catch (error) {
        console.error("采纳故宫候选壁纸失败", error);
        setWallpaperActionTone("error");
        setWallpaperActionMessage(describeWallpaperError(error));
      } finally {
        setWallpaperActionPending("");
      }
    },
    [
      loadLocalWallpapers,
      loadPalaceStagingWallpapers,
      palaceRefreshStatus,
      selectedPalaceStagingWallpaper,
    ],
  );

  const handlePalacePageChange = useCallback(
    async (targetPage: number) => {
      if (palaceStagingLoading || palaceRefreshStatus?.state === "running") {
        return;
      }
      const nextPage = Math.max(1, targetPage);
      if (nextPage === palaceStagingPage) return;
      await refreshPalaceStagingBatch("page", nextPage);
    },
    [palaceRefreshStatus, palaceStagingLoading, palaceStagingPage, refreshPalaceStagingBatch],
  );

  const handleEnterPalaceBatchMode = useCallback(() => {
    if (palaceRefreshStatus?.state === "running") return;
    setPalaceBatchMode(true);
    setSelectedPalaceStageSources([]);
    setWallpaperActionTone("muted");
    setWallpaperActionMessage("已进入批量模式，可以勾选多张故宫候选图一起加入本地库。");
  }, [palaceRefreshStatus]);

  const handleExitPalaceBatchMode = useCallback(() => {
    setPalaceBatchMode(false);
    setSelectedPalaceStageSources([]);
  }, []);

  const handleTogglePalaceStageSelection = useCallback(
    (sourceUrl: string) => {
      if (palaceRefreshStatus?.state === "running") return;
      setSelectedPalaceStagingSource(sourceUrl);
      if (!palaceBatchMode) return;
      setSelectedPalaceStageSources((prev) =>
        prev.includes(sourceUrl)
          ? prev.filter((item) => item !== sourceUrl)
          : [...prev, sourceUrl],
      );
    },
    [palaceBatchMode, palaceRefreshStatus],
  );

  const handleSelectAllPalaceStage = useCallback(() => {
    if (palaceRefreshStatus?.state === "running") return;
    setSelectedPalaceStageSources(
      palaceStagingWallpapers.map((item) => item.sourceUrl),
    );
  }, [palaceRefreshStatus, palaceStagingWallpapers]);

  const handleClearPalaceStageSelection = useCallback(() => {
    if (palaceRefreshStatus?.state === "running") return;
    setSelectedPalaceStageSources([]);
  }, [palaceRefreshStatus]);

  const handlePromoteSelectedPalaceWallpapers = useCallback(async () => {
    if (palaceRefreshStatus?.state === "running") return;
    if (selectedPalaceStageSources.length === 0) {
      setWallpaperActionTone("error");
      setWallpaperActionMessage("请先选择至少一张故宫候选壁纸。");
      return;
    }
    setWallpaperActionPending("promote-bulk");
    setWallpaperActionTone("muted");
    setWallpaperActionMessage(
      `正在把选中的 ${selectedPalaceStageSources.length} 张故宫候选图加入本地库。`,
    );
    try {
      const result = await invoke<PalaceStagingBatchResult>(
        "promote_palace_staging_wallpapers",
        { sourceUrls: selectedPalaceStageSources },
      );
      applyPalaceStagingResult(result);
      setSelectedPalaceStageSources([]);
      await loadLocalWallpapers();
      setWallpaperActionTone("success");
      setWallpaperActionMessage(
        result.processedCount > 0 && result.skippedCount > 0
          ? `已加入 ${result.processedCount} 张，另有 ${result.skippedCount} 张本来就在本地库中。`
          : result.processedCount > 0
            ? `已把 ${result.processedCount} 张故宫候选图加入本地壁纸库。`
            : `选中的 ${result.skippedCount} 张都已在本地库中，候选区已同步移除。`,
      );
    } catch (error) {
      console.error("批量采纳故宫候选壁纸失败", error);
      setWallpaperActionTone("error");
      setWallpaperActionMessage(describeWallpaperError(error));
    } finally {
      setWallpaperActionPending("");
    }
  }, [
    applyPalaceStagingResult,
    loadLocalWallpapers,
    palaceRefreshStatus,
    selectedPalaceStageSources,
  ]);

  const handlePromoteAllPalaceWallpapers = useCallback(async () => {
    if (palaceRefreshStatus?.state === "running") return;
    if (palaceStagingWallpapers.length === 0) {
      setWallpaperActionTone("error");
      setWallpaperActionMessage("当前页没有可加入本地库的故宫候选壁纸。");
      return;
    }
    setWallpaperActionPending("promote-bulk");
    setWallpaperActionTone("muted");
    setWallpaperActionMessage(
      `正在把当前页的 ${palaceStagingWallpapers.length} 张故宫候选图全部加入本地库。`,
    );
    try {
      const result = await invoke<PalaceStagingBatchResult>(
        "promote_palace_staging_wallpapers",
        { sourceUrls: palaceStagingWallpapers.map((item) => item.sourceUrl) },
      );
      applyPalaceStagingResult(result);
      setSelectedPalaceStageSources([]);
      await loadLocalWallpapers();
      setWallpaperActionTone("success");
      setWallpaperActionMessage(
        result.processedCount > 0 && result.skippedCount > 0
          ? `当前页已加入 ${result.processedCount} 张，另有 ${result.skippedCount} 张原本就在本地库中。`
          : result.processedCount > 0
            ? `当前页这批故宫候选图已经全部加入本地壁纸库。`
            : `当前页候选图原本都在本地库中，候选区已同步清空。`,
      );
    } catch (error) {
      console.error("全部采纳故宫候选壁纸失败", error);
      setWallpaperActionTone("error");
      setWallpaperActionMessage(describeWallpaperError(error));
    } finally {
      setWallpaperActionPending("");
    }
  }, [
    applyPalaceStagingResult,
    loadLocalWallpapers,
    palaceRefreshStatus,
    palaceStagingWallpapers,
  ]);

  const handleFixWallpaper = useCallback(
    async (sourceUrl: string) => {
      setWallpaperActionPending("fixed-local");
      setWallpaperActionTone("muted");
      setWallpaperActionMessage("正在固定这张壁纸。");
      try {
        await invoke("set_fixed_wallpaper", { sourceUrl });
        setWallpaperActionTone("success");
        setWallpaperActionMessage("这张壁纸已经固定，后续每次锁屏都会先显示它。");
        await loadLocalWallpapers();
        setSelectedLocalWallpaperSource(sourceUrl);
      } catch (error) {
        console.error("固定壁纸失败", error);
        setWallpaperActionTone("error");
        setWallpaperActionMessage(describeWallpaperError(error));
      } finally {
        setWallpaperActionPending("");
      }
    },
    [loadLocalWallpapers],
  );

  const handleClearFixedWallpaper = useCallback(async () => {
    setWallpaperActionPending("clear-fixed");
    setWallpaperActionTone("muted");
    setWallpaperActionMessage("正在取消固定壁纸。");
    try {
      await invoke("clear_fixed_wallpaper");
      setWallpaperActionTone("success");
      setWallpaperActionMessage("已经取消固定，后续锁屏会恢复顺序展示。");
      await loadLocalWallpapers();
    } catch (error) {
      console.error("取消固定壁纸失败", error);
      setWallpaperActionTone("error");
      setWallpaperActionMessage(describeWallpaperError(error));
    } finally {
      setWallpaperActionPending("");
    }
  }, [loadLocalWallpapers]);

  const handleDeleteLocalWallpaper = useCallback(
    async (sourceUrl: string) => {
      const targetIndex = localWallpapers.findIndex(
        (item) => item.sourceUrl === sourceUrl,
      );
      if (targetIndex < 0) return;
      const target = localWallpapers[targetIndex];
      const fallbackSource =
        localWallpapers[targetIndex + 1]?.sourceUrl ??
        localWallpapers[targetIndex - 1]?.sourceUrl ??
        null;
      const confirmed = window.confirm(
        "删除后会永久移除本地壁纸文件，且不会进入回收站。确认删除这张壁纸吗？",
      );
      if (!confirmed) return;

      setWallpaperActionPending("delete");
      setWallpaperActionTone("muted");
      setWallpaperActionMessage("正在删除本地壁纸。");
      try {
        await invoke("delete_local_wallpaper", { sourceUrl });
        const items = (await loadLocalWallpapers()) ?? [];
        setSelectedLocalWallpaperSource(
          items.some((item) => item.sourceUrl === fallbackSource)
            ? fallbackSource
            : items[0]?.sourceUrl ?? null,
        );
        setWallpaperActionTone("success");
        setWallpaperActionMessage(
          target.isFixed
            ? "已删除固定壁纸，后续锁屏会恢复顺序展示。"
            : "已删除这张本地壁纸。",
        );
      } catch (error) {
        console.error("删除本地壁纸失败", error);
        setWallpaperActionTone("error");
        setWallpaperActionMessage(describeWallpaperError(error));
      } finally {
        setWallpaperActionPending("");
      }
    },
    [loadLocalWallpapers, localWallpapers],
  );

  const handleExitRest = useCallback(() => {
    if (exitInProgressRef.current) return;
    exitInProgressRef.current = true;
    invoke("log_app", { message: "前端退出休息: start" }).catch(() => undefined);
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
    invoke("log_app", { message: "前端退出休息: end" }).catch(() => undefined);
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

  useEffect(() => {
    exitRestRef.current = handleExitRest;
  }, [handleExitRest]);

  useEffect(() => {
    togglePauseRef.current = handleTogglePause;
  }, [handleTogglePause]);

  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (showLockScreen) {
      exitInProgressRef.current = false;
    }
  }, [showLockScreen]);

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
    if (isLockWindow || activeView !== "wallpapers") return;
    void loadWallpaperStorageSettings();
    void loadLocalWallpapers();
    void (async () => {
      const settings = await loadUnsplashSettings();
      const effectiveConfigured =
        settings?.effectiveConfigured ??
        (unsplashSettings?.effectiveConfigured ?? false);
      if (effectiveConfigured) {
        if (!unsplashSearchResult && !unsplashSearchLoading && !unsplashSearchError) {
          await runUnsplashSearch(unsplashSearchInput, 1);
        }
      }
      // 故宫壁纸不再自动触发获取，必须用户手动点击按钮
    })();
  }, [
    activeView,
    isLockWindow,
    loadLocalWallpapers,
    loadUnsplashSettings,
    loadWallpaperStorageSettings,
    runUnsplashSearch,
    unsplashSearchError,
    unsplashSearchInput,
    unsplashSearchLoading,
    unsplashSearchResult,
    unsplashSettings?.effectiveConfigured,
  ]);

  useEffect(() => {
    if (isLockWindow || activeView !== "wallpapers") return;
    if (
      activeRemoteSource !== "palace" &&
      unsplashSettings?.effectiveConfigured !== false
    ) {
      return;
    }
    void loadPalaceRefreshStatus();
  }, [
    activeRemoteSource,
    activeView,
    isLockWindow,
    loadPalaceRefreshStatus,
    unsplashSettings?.effectiveConfigured,
  ]);

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
      invoke("log_app", { message: "前端请求关闭锁屏" }).catch(() => undefined);
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
          exitRestRef.current();
        } else if (event.payload === "toggle_pause") {
          togglePauseRef.current();
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
  }, [isLockWindow]);

  useEffect(() => {
    if (isLockWindow) return;
    let unlisten: (() => void) | undefined;
    getCurrentWebviewWindow()
      .listen<WallpaperStorageSettings>("wallpaper-storage-updated", () => {
        void loadWallpaperStorageSettings();
        void loadLocalWallpapers();
        void loadPalaceStagingWallpapers();
        void loadPalaceRefreshStatus();
        setPalaceStagingBootstrapped(false);
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((error) => console.error("监听壁纸目录变更失败", error));
    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [
    isLockWindow,
    loadLocalWallpapers,
    loadPalaceRefreshStatus,
    loadPalaceStagingWallpapers,
    loadWallpaperStorageSettings,
  ]);

  useEffect(() => {
    if (isLockWindow) return;
    let unlisten: (() => void) | undefined;
    getCurrentWebviewWindow()
      .listen<PalaceStagingRefreshStatus>("palace-staging-refresh", (event) => {
        applyPalaceRefreshStatus(event.payload);
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((error) => console.error("监听故宫后台刷新事件失败", error));
    return () => {
      if (unlisten) {
        unlisten();
      }
    };
  }, [applyPalaceRefreshStatus, isLockWindow]);

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
    let unlisten: (() => void) | undefined;
    void reloadLockWallpaperFromStorage().catch((error) => {
      if (!active) return;
      console.error("获取锁屏壁纸失败", error);
      setLockBackgroundUrl(null);
      setLockSessionFixedMode(false);
      setLockWallpaperHistory([]);
      setLockWallpaperIndex(0);
    });
    getCurrentWebviewWindow()
      .listen<WallpaperStorageSettings>("wallpaper-storage-updated", () => {
        if (!active) return;
        void reloadLockWallpaperFromStorage().catch((error) =>
          console.error("切换目录后刷新锁屏壁纸失败", error),
        );
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((error) => console.error("监听壁纸目录更新失败", error));
    return () => {
      active = false;
      if (unlisten) {
        unlisten();
      }
    };
  }, [isLockWindow, reloadLockWallpaperFromStorage]);

  const handleNextWallpaper = useCallback(() => {
    if (!isLockWindow) return;
    if (lockWallpaperIndex < lockWallpaperHistory.length - 1) {
      const nextIndex = lockWallpaperIndex + 1;
      setLockWallpaperIndex(nextIndex);
      setLockBackgroundUrl(lockWallpaperHistory[nextIndex]);
      return;
    }
    if (lockSessionFixedMode) {
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
  }, [isLockWindow, lockSessionFixedMode, lockWallpaperHistory, lockWallpaperIndex]);

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
  const isUnsplashConfigured =
    unsplashSettings?.effectiveConfigured ??
    unsplashSearchResult?.configured ??
    true;
  const unsplashConfigSourceLabel = describeUnsplashConfigSource(
    unsplashSettings?.configSource ?? "none",
  );
  const canLoadPrevUnsplashPage = (unsplashSearchResult?.page ?? 1) > 1;
  const canLoadNextUnsplashPage = unsplashSearchResult?.hasNextPage ?? false;
  const refreshSourceLabel = isUnsplashConfigured ? "Unsplash" : "故宫壁纸";
  const refreshActionLabel = isUnsplashConfigured
    ? `下载 ${refreshSourceLabel} 新壁纸`
    : "获取故宫候选壁纸";
  const hasPalaceCandidates = palaceStagingWallpapers.length > 0;
  const isPalaceRefreshRunning = palaceRefreshStatus?.state === "running";
  const palaceRefreshTargetPage =
    palaceRefreshStatus?.targetPage ?? palaceStagingPage;
  const palaceControlsDisabled = palaceStagingLoading || isPalaceRefreshRunning;
  const palaceRefreshProgressText = isPalaceRefreshRunning
    ? palaceRefreshStatus?.totalEntries &&
      palaceRefreshStatus.totalEntries > 0
      ? `正在获取第 ${palaceRefreshTargetPage} 页，已处理 ${palaceRefreshStatus.processedEntries}/${palaceRefreshStatus.totalEntries}`
      : `正在获取第 ${palaceRefreshTargetPage} 页...`
    : "";
  const isPalacePageProcessed =
    palaceStagingBootstrapped &&
    !palaceControlsDisabled &&
    activeRemoteSource === "palace" &&
    palaceStagingWallpapers.length === 0;
  const selectedLocalWallpaperUrl = selectedLocalWallpaper
    ? convertFileSrc(selectedLocalWallpaper.path)
    : null;
  const selectedPalaceStagingWallpaperUrl = selectedPalaceStagingWallpaper
    ? convertFileSrc(selectedPalaceStagingWallpaper.path)
    : null;
  const isUsingDefaultWallpaperDir = wallpaperStorageSettings?.isDefault ?? true;

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

  const wallpaperConsoleContent = (
    <section className="wallpaper-console">
      <div className="card wallpaper-console__toolbar">
        <div>
          <p className="card__eyebrow">壁纸控制台</p>
          <h2>在线搜索与本地管理</h2>
          <p className="helper-text">
            Unsplash 保持在线搜索；故宫会先在后台抓一批候选图，再用本地文件预览和挑选。
          </p>
        </div>
        <div className="wallpaper-source-tabs">
          <button
            className={`view-tab ${
              activeRemoteSource === "unsplash" ? "view-tab--active" : ""
            }`}
            type="button"
            disabled={!isUnsplashConfigured}
            onClick={() => {
              setActiveRemoteSource("unsplash");
              if (!unsplashSearchResult && !unsplashSearchLoading) {
                void runUnsplashSearch(unsplashSearchInput, 1);
              }
            }}
          >
            Unsplash
          </button>
          <button
            className={`view-tab ${
              activeRemoteSource === "palace" ? "view-tab--active" : ""
            }`}
            type="button"
            onClick={() => {
              setActiveRemoteSource("palace");
            }}
          >
            故宫壁纸
          </button>
        </div>
        {!isUnsplashConfigured && (
          <p className="helper-text">
            Unsplash 需要先配置 Access Key。你可以在“壁纸设置”里填写，当前会自动使用故宫来源。
          </p>
        )}
        {activeRemoteSource === "unsplash" ? (
          <form
            className="wallpaper-search"
            onSubmit={(event) => void handleUnsplashSearchSubmit(event)}
          >
            <input
              className="wallpaper-search__input"
              type="text"
              placeholder="搜索关键字，例如 mountain、forest、ocean"
              value={unsplashSearchInput}
              onChange={(event) => setUnsplashSearchInput(event.target.value)}
              disabled={!isUnsplashConfigured}
            />
            <button
              className="btn btn--ghost"
              type="submit"
              disabled={unsplashSearchLoading || !isUnsplashConfigured}
            >
              {unsplashSearchLoading ? "搜索中..." : "搜索壁纸"}
            </button>
          </form>
        ) : (
          <div className="empty-state wallpaper-source-note">
            <h3>故宫来源改为本地候选批次</h3>
            <p>先由后台抓取一批可用桌面图，再在这里本地预览和挑选，避免页面持续在线请求。</p>
          </div>
        )}
        <div className="wallpaper-console__controls">
          {activeRemoteSource === "unsplash" ? (
            <div className="page-controls">
              <button
                className="btn btn--soft"
                type="button"
                onClick={() =>
                  void runUnsplashSearch(
                    unsplashSearchQuery,
                    Math.max(1, (unsplashSearchResult?.page ?? 1) - 1),
                  )
                }
                disabled={!canLoadPrevUnsplashPage || unsplashSearchLoading}
              >
                上一页
              </button>
              <span className="page-controls__text">
                第 {unsplashSearchResult?.page ?? 1} 页
                {unsplashSearchResult?.totalPages
                  ? ` / ${unsplashSearchResult.totalPages}`
                  : ""}
              </span>
              <button
                className="btn btn--soft"
                type="button"
                onClick={() =>
                  void runUnsplashSearch(
                    unsplashSearchQuery,
                    (unsplashSearchResult?.page ?? 1) + 1,
                  )
                }
                disabled={!canLoadNextUnsplashPage || unsplashSearchLoading}
              >
                下一页
              </button>
            </div>
          ) : (
            <div className="page-controls">
              <button
                className="btn btn--soft"
                type="button"
                onClick={() => void handlePalacePageChange(palaceStagingPage - 1)}
                disabled={!palaceStagingHasPrevPage || palaceControlsDisabled}
              >
                上一页
              </button>
              <span className="page-controls__text">
                第 {palaceStagingPage} 页 / {palaceStagingMaxPage}
              </span>
              <button
                className="btn btn--soft"
                type="button"
                onClick={() => void handlePalacePageChange(palaceStagingPage + 1)}
                disabled={!palaceStagingHasNextPage || palaceControlsDisabled}
              >
                下一页
              </button>
              <span className="page-controls__text">
                当前候选 {palaceStagingWallpapers.length} 张
              </span>
            </div>
          )}
          <div className="wallpaper-console__actions">
            <button
              className="btn btn--soft"
              type="button"
              onClick={() => setWallpaperSettingsOpen((prev) => !prev)}
              disabled={isPalaceRefreshRunning}
            >
              {wallpaperSettingsOpen ? "收起设置" : "壁纸设置"}
            </button>
            {activeRemoteSource === "palace" ? (
              palaceBatchMode ? (
                <>
                  <span className="page-controls__text">
                    已选 {selectedPalaceStageCount} 张
                  </span>
                  <button
                    className="btn btn--soft"
                    type="button"
                    onClick={handleSelectAllPalaceStage}
                    disabled={!hasPalaceCandidates || palaceControlsDisabled}
                  >
                    全选本页
                  </button>
                  <button
                    className="btn btn--soft"
                    type="button"
                    onClick={handleClearPalaceStageSelection}
                    disabled={
                      selectedPalaceStageCount === 0 || palaceControlsDisabled
                    }
                  >
                    取消选择
                  </button>
                  <button
                    className="btn btn--primary"
                    type="button"
                    onClick={() => void handlePromoteSelectedPalaceWallpapers()}
                    disabled={
                      selectedPalaceStageCount === 0 ||
                      wallpaperActionPending !== "" ||
                      palaceControlsDisabled
                    }
                  >
                    {wallpaperActionPending === "promote-bulk"
                      ? "处理中..."
                      : "批量加入本地壁纸库"}
                  </button>
                  <button
                    className="btn btn--ghost"
                    type="button"
                    onClick={handleExitPalaceBatchMode}
                    disabled={palaceControlsDisabled}
                  >
                    退出批量模式
                  </button>
                </>
              ) : (
                <>
                  <button
                    className="btn btn--soft"
                    type="button"
                    onClick={() => void handlePromoteAllPalaceWallpapers()}
                    disabled={
                      !hasPalaceCandidates ||
                      palaceControlsDisabled ||
                      wallpaperActionPending !== ""
                    }
                  >
                    {wallpaperActionPending === "promote-bulk"
                      ? "处理中..."
                      : "全部加入本地壁纸库"}
                  </button>
                  <button
                    className="btn btn--soft"
                    type="button"
                    onClick={handleEnterPalaceBatchMode}
                    disabled={!hasPalaceCandidates || palaceControlsDisabled}
                  >
                    批量选择
                  </button>
                  <button
                    className="btn btn--primary"
                    type="button"
                    onClick={() =>
                      void refreshPalaceStagingBatch("manual", palaceStagingPage)
                    }
                    disabled={palaceControlsDisabled}
                  >
                    {isPalaceRefreshRunning ? "获取中..." : "刷新当前页"}
                  </button>
                  {isPalaceRefreshRunning && (
                    <button
                      className="btn btn--ghost"
                      type="button"
                      onClick={() => void handleCancelPalaceRefresh()}
                    >
                      取消
                    </button>
                  )}
                </>
              )
            ) : (
              <button
                className="btn btn--primary"
                type="button"
                onClick={handleRefreshWallpaper}
                disabled={wallpaperRefreshPending}
              >
                {wallpaperRefreshPending ? "下载中..." : refreshActionLabel}
              </button>
            )}
          </div>
        </div>
        {activeRemoteSource === "palace" && palaceRefreshProgressText && (
          <p className="helper-text">{palaceRefreshProgressText}</p>
        )}

        {wallpaperSettingsOpen && (
          <div className="wallpaper-storage-panel">
            <div className="wallpaper-storage-panel__meta">
              <div>
                <p className="card__eyebrow">当前保存位置</p>
                <strong>{wallpaperStorageSettings?.currentDir || "正在读取..."}</strong>
              </div>
              <span className="helper-text">
                {isUsingDefaultWallpaperDir
                  ? "当前使用默认缓存目录"
                  : "当前使用自定义目录"}
              </span>
            </div>
            <label className="wallpaper-storage-field">
              <span>壁纸保存目录</span>
              <input
                className="wallpaper-search__input wallpaper-storage-field__input"
                type="text"
                value={wallpaperStorageInput}
                onChange={(event) => setWallpaperStorageInput(event.target.value)}
                placeholder="输入绝对路径，或使用右侧按钮选择文件夹"
                disabled={wallpaperStoragePending || isPalaceRefreshRunning}
              />
            </label>
            <div className="wallpaper-storage-panel__actions">
              <button
                className="btn btn--soft"
                type="button"
                onClick={() => void handlePickWallpaperFolder()}
                disabled={wallpaperStoragePending || isPalaceRefreshRunning}
              >
                选择文件夹
              </button>
              <button
                className="btn btn--primary"
                type="button"
                onClick={() => void handleSaveWallpaperStorageDir()}
                disabled={wallpaperStoragePending || isPalaceRefreshRunning}
              >
                {wallpaperStoragePending ? "保存中..." : "保存目录"}
              </button>
              <button
                className="btn btn--ghost"
                type="button"
                onClick={() => void handleResetWallpaperStorageDir()}
                disabled={
                  wallpaperStoragePending ||
                  isUsingDefaultWallpaperDir ||
                  isPalaceRefreshRunning
                }
              >
                恢复默认
              </button>
            </div>
            <div className="wallpaper-storage-panel__meta wallpaper-storage-panel__meta--stack">
              <p className={`helper-text helper-text--${wallpaperStorageTone}`}>
                {wallpaperStorageMessage}
              </p>
              <p className="helper-text">
                默认目录：{wallpaperStorageSettings?.defaultDir || "正在读取..."}
              </p>
            </div>
            <div className="wallpaper-storage-panel__divider" />
            <div className="wallpaper-storage-panel__meta">
              <div>
                <p className="card__eyebrow">Unsplash Access Key</p>
                <strong>
                {unsplashSettings?.isBuiltIn
                  ? "内置 Key 已激活"
                  : isUnsplashConfigured
                    ? "已配置"
                    : "未配置"}
              </strong>
              </div>
              <span className="helper-text">当前来源：{unsplashConfigSourceLabel}</span>
            </div>
            <div className="wallpaper-storage-panel__meta wallpaper-storage-panel__meta--stack">
              {unsplashSettings?.isBuiltIn ? (
                <p className="helper-text helper-text--success">
                  正在使用程序内置的 Unsplash Access Key，无需额外配置。
                </p>
              ) : null}
              {unsplashSettings?.hasStoredKey ? (
                <p className="helper-text">
                  已保存应用内 Key：{unsplashSettings.maskedStoredKey || "已保存"}
                </p>
              ) : unsplashSettings?.isBuiltIn ? (
                <p className="helper-text">
                  内置 Key（{unsplashSettings.maskedStoredKey || "已配置"}）。
                  你可以保存自己的 Key 来覆盖内置 Key。
                </p>
              ) : (
                <p className="helper-text">当前没有保存应用内 Key。</p>
              )}
              <p className="helper-text">
                这里只需要 Access Key，不需要 Application ID 和 Secret Key。
              </p>
            </div>
            <label className="wallpaper-storage-field">
              <span>新的 Unsplash Access Key</span>
              <input
                className="wallpaper-search__input wallpaper-storage-field__input"
                type="password"
                value={unsplashAccessKeyInput}
                onChange={(event) => setUnsplashAccessKeyInput(event.target.value)}
                placeholder="粘贴新的 Access Key 后保存"
                disabled={unsplashSettingsPending || isPalaceRefreshRunning}
              />
            </label>
            <div className="wallpaper-storage-panel__actions">
              <button
                className="btn btn--primary"
                type="button"
                onClick={() => void handleSaveUnsplashAccessKey()}
                disabled={unsplashSettingsPending || isPalaceRefreshRunning}
              >
                {unsplashSettingsPending ? "保存中..." : "保存 Access Key"}
              </button>
              <button
                className="btn btn--ghost"
                type="button"
                onClick={() => void handleClearUnsplashAccessKey()}
                disabled={
                  unsplashSettingsPending ||
                  isPalaceRefreshRunning ||
                  !unsplashSettings?.hasStoredKey
                }
              >
                {unsplashSettings?.hasStoredKey
                  ? "清除已保存 Key"
                  : "清除内置 Key"}
              </button>
            </div>
            <div className="wallpaper-storage-panel__meta wallpaper-storage-panel__meta--stack">
              <p className={`helper-text helper-text--${unsplashSettingsTone}`}>
                {unsplashSettingsMessage}
              </p>
            </div>
          </div>
        )}
      </div>

      <div className="wallpaper-console__grid">
        <div className="card wallpaper-results-card">
          <div className="card__header">
            <div>
              <p className="card__eyebrow">
                {activeRemoteSource === "unsplash" ? "在线图库" : "故宫候选库"}
              </p>
              <h2>
                {activeRemoteSource === "unsplash"
                  ? "Unsplash 在线浏览"
                  : "故宫候选批次"}
              </h2>
            </div>
            <span className="helper-text">
              {activeRemoteSource === "unsplash"
                ? `每页 ${WALLPAPER_PAGE_SIZE} 张`
                : `第 ${palaceStagingPage} 页候选 ${palaceStagingWallpapers.length} 张`}
            </span>
          </div>
          {activeRemoteSource === "palace" && palaceStagingError && (
            <p className="helper-text helper-text--error">{palaceStagingError}</p>
          )}
          {activeRemoteSource === "palace" && palaceBatchMode && (
            <p className="helper-text">
              批量模式已开启，点击候选卡片即可切换选中状态，右侧仍会保持预览。
            </p>
          )}
          {activeRemoteSource === "unsplash" && !isUnsplashConfigured ? (
            <div className="empty-state">
              <h3>还没有配置 Unsplash</h3>
              <p>
                {unsplashSearchResult?.errorMessage ??
                  "先在壁纸设置里填写 Access Key，或继续使用环境变量。"}
              </p>
            </div>
          ) : activeRemoteSource === "unsplash" ? (
            <>
              {unsplashSearchError && (
                <p className="helper-text helper-text--error">{unsplashSearchError}</p>
              )}
              {unsplashSearchLoading ? (
                <div className="empty-state">
                  <h3>正在加载在线壁纸</h3>
                  <p>正在加载 Unsplash 当前页数据。</p>
                </div>
              ) : unsplashSearchResult?.items.length ? (
                <div className="wallpaper-grid">
                  {unsplashSearchResult.items.map((item) => (
                    <button
                      key={item.id}
                      className={`wallpaper-tile ${
                        selectedUnsplashWallpaper?.id === item.id
                          ? "wallpaper-tile--selected"
                          : ""
                      }`}
                      type="button"
                      onClick={() => setSelectedUnsplashWallpaperId(item.id)}
                    >
                      <img src={item.thumbUrl} alt={describeWallpaper(item)} />
                      <div className="wallpaper-tile__meta">
                        <strong>{describeWallpaper(item)}</strong>
                        <span>
                          {item.width} x {item.height}
                        </span>
                        <span>{getRemoteSourceLabel(item.source)}</span>
                        <span>{item.creditName}</span>
                      </div>
                    </button>
                  ))}
                </div>
              ) : (
                <div className="empty-state">
                  <h3>没有找到符合条件的壁纸</h3>
                  <p>试试更宽泛一点的关键词，或者直接浏览默认壁纸主题。</p>
                </div>
              )}
            </>
          ) : hasPalaceCandidates ? (
            <div className="wallpaper-grid">
              {palaceStagingWallpapers.map((item) => (
                <button
                  key={item.sourceUrl}
                  className={`wallpaper-tile ${
                    selectedPalaceStagingWallpaper?.sourceUrl === item.sourceUrl
                      ? "wallpaper-tile--selected"
                      : ""
                  } ${
                    selectedPalaceStageSources.includes(item.sourceUrl)
                      ? "wallpaper-tile--checked"
                      : ""
                  } ${
                    palaceBatchMode ? "wallpaper-tile--batch" : ""
                  }`}
                  type="button"
                  onClick={() => handleTogglePalaceStageSelection(item.sourceUrl)}
                  disabled={isPalaceRefreshRunning}
                >
                  {palaceBatchMode && (
                    <span
                      className={`wallpaper-tile__checkbox ${
                        selectedPalaceStageSources.includes(item.sourceUrl)
                          ? "wallpaper-tile__checkbox--checked"
                          : ""
                      }`}
                    >
                      {selectedPalaceStageSources.includes(item.sourceUrl)
                        ? "已选"
                        : "选择"}
                    </span>
                  )}
                  <img src={convertFileSrc(item.path)} alt={item.title} />
                  <div className="wallpaper-tile__meta">
                    <strong>{item.title || "故宫壁纸"}</strong>
                    <span>
                      {item.width} x {item.height}
                    </span>
                    <span>故宫候选</span>
                    <span>{item.creditName}</span>
                  </div>
                </button>
              ))}
            </div>
          ) : palaceStagingLoading || isPalaceRefreshRunning ? (
            <div className="empty-state">
              <h3>正在获取故宫候选壁纸</h3>
              <p>
                {isPalaceRefreshRunning
                  ? `后台正在抓取并缓存第 ${palaceRefreshTargetPage} 页的桌面横屏图片。`
                  : `正在读取第 ${palaceStagingPage} 页的故宫候选缓存。`}
              </p>
            </div>
          ) : isPalacePageProcessed ? (
            <div className="empty-state">
              <h3>当前页候选已处理完成</h3>
              <p>
                第 {palaceStagingPage} 页里可加入本地库的候选图已经处理完了，
                {palaceStagingHasNextPage ? "可以翻到下一页继续挑选。" : "也可以手动刷新当前页再试一次。"}
              </p>
            </div>
          ) : (
            <div className="empty-state">
              <h3>还没有故宫候选壁纸</h3>
              <p>点击上面的“刷新当前页”，后台抓到图片后会直接在这里本地预览。</p>
            </div>
          )}
        </div>

        <div className="card wallpaper-preview-card">
          {activeRemoteSource === "unsplash" && selectedUnsplashWallpaper ? (
            <>
              <div className="wallpaper-preview__image">
                <img
                  src={selectedUnsplashWallpaper.previewUrl}
                  alt={describeWallpaper(selectedUnsplashWallpaper)}
                />
              </div>
              <div className="wallpaper-preview__body">
                <p className="card__eyebrow">在线预览</p>
                <h2>{describeWallpaper(selectedUnsplashWallpaper)}</h2>
                <p className="helper-text">
                  来源 {getRemoteSourceLabel(selectedUnsplashWallpaper.source)}
                </p>
                <p className="helper-text">
                  {selectedUnsplashWallpaper.width} x {selectedUnsplashWallpaper.height}
                </p>
                <p className="helper-text">
                  来源信息{" "}
                  <a
                    href={selectedUnsplashWallpaper.creditUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {selectedUnsplashWallpaper.creditName}
                  </a>
                </p>
                <p className={`helper-text helper-text--${wallpaperActionTone}`}>
                  {wallpaperActionMessage}
                </p>
                <div className="preview-actions preview-actions--stack">
                  <button
                    className="btn btn--primary"
                    type="button"
                    onClick={() => void handleDownloadWallpaper(true)}
                    disabled={wallpaperActionPending !== ""}
                  >
                    {wallpaperActionPending === "fixed"
                      ? "处理中..."
                      : "下载并固定"}
                  </button>
                  <button
                    className="btn btn--ghost"
                    type="button"
                    onClick={() => void handleDownloadWallpaper(false)}
                    disabled={wallpaperActionPending !== ""}
                  >
                    {wallpaperActionPending === "download"
                      ? "处理中..."
                      : "仅下载到本地"}
                  </button>
                </div>
              </div>
            </>
          ) : activeRemoteSource === "palace" &&
            selectedPalaceStagingWallpaper &&
            selectedPalaceStagingWallpaperUrl ? (
            <>
              <div className="wallpaper-preview__image">
                <img
                  src={selectedPalaceStagingWallpaperUrl}
                  alt={selectedPalaceStagingWallpaper.title}
                />
              </div>
              <div className="wallpaper-preview__body">
                <p className="card__eyebrow">故宫候选预览</p>
                <h2>{selectedPalaceStagingWallpaper.title || "故宫壁纸"}</h2>
                <p className="helper-text">
                  这是一张已经抓到本地缓存的故宫候选图，确认喜欢后再加入壁纸库。
                </p>
                <p className="helper-text">
                  {selectedPalaceStagingWallpaper.width} x{" "}
                  {selectedPalaceStagingWallpaper.height}
                </p>
                <p className="helper-text">
                  来源信息{" "}
                  <a
                    href={selectedPalaceStagingWallpaper.creditUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {selectedPalaceStagingWallpaper.creditName}
                  </a>
                </p>
                <p className={`helper-text helper-text--${wallpaperActionTone}`}>
                  {wallpaperActionMessage}
                </p>
                {palaceBatchMode ? (
                  <p className="helper-text">
                    当前处于批量模式，请使用上方工具栏执行全选、批量加入或退出批量模式。
                  </p>
                ) : (
                  <div className="preview-actions preview-actions--stack">
                    <button
                      className="btn btn--primary"
                      type="button"
                      onClick={() => void handlePromotePalaceWallpaper(false)}
                      disabled={wallpaperActionPending !== "" || palaceControlsDisabled}
                    >
                      {wallpaperActionPending === "promote"
                        ? "处理中..."
                        : "加入本地壁纸库"}
                    </button>
                    <button
                      className="btn btn--ghost"
                      type="button"
                      onClick={() => void handlePromotePalaceWallpaper(true)}
                      disabled={wallpaperActionPending !== "" || palaceControlsDisabled}
                    >
                      {wallpaperActionPending === "promote-fixed"
                        ? "处理中..."
                        : "加入并固定"}
                    </button>
                  </div>
                )}
              </div>
            </>
          ) : (
            <div className="empty-state">
              <h3>选择一张壁纸开始预览</h3>
              <p>
                {activeRemoteSource === "unsplash"
                  ? "右侧会显示大图、作者信息，以及下载动作。"
                  : "右侧会显示故宫候选壁纸的大图预览，并决定是否加入本地库。"}
              </p>
            </div>
          )}
        </div>
      </div>

      <div className="card wallpaper-library">
        <div className="card__header">
          <div>
            <p className="card__eyebrow">本地壁纸库</p>
            <h2>浏览已缓存壁纸</h2>
          </div>
          <button
            className="btn btn--soft"
            type="button"
            onClick={() => void loadLocalWallpapers()}
            disabled={localWallpapersLoading}
          >
            {localWallpapersLoading ? "刷新中..." : "刷新本地库"}
          </button>
        </div>
        {localWallpapersError && (
          <p className="helper-text helper-text--error">{localWallpapersError}</p>
        )}
        {localWallpapersLoading ? (
          <div className="empty-state">
            <h3>正在加载本地壁纸</h3>
            <p>正在读取当前缓存的壁纸文件。</p>
          </div>
        ) : localWallpapers.length === 0 ? (
          <div className="empty-state">
            <h3>本地壁纸库还是空的</h3>
            <p>先从上面的 Unsplash 在线图库，或故宫候选批次里加入几张喜欢的壁纸吧。</p>
          </div>
        ) : (
          <>
            {selectedLocalWallpaper && selectedLocalWallpaperUrl && (
              <div className="library-featured">
                <div className="library-featured__image">
                  <img
                    src={selectedLocalWallpaperUrl}
                    alt={selectedLocalWallpaper.authorName || "本地壁纸"}
                  />
                </div>
                <div className="library-featured__body">
                  <p className="card__eyebrow">当前选中</p>
                  <h2>
                    {selectedLocalWallpaper.isFixed
                      ? "固定锁屏壁纸"
                      : "顺序展示壁纸"}
                  </h2>
                  <p className="helper-text">
                    来源 {getLocalSourceLabel(selectedLocalWallpaper.sourceKind)}，加入于{" "}
                    {formatAddedAt(selectedLocalWallpaper.addedAt)}
                  </p>
                  <p className="helper-text">
                    来源信息 {selectedLocalWallpaper.authorName || "未知来源"}
                  </p>
                  <p className="helper-text">
                    {selectedLocalWallpaper.isFixed
                      ? "这张图已经固定，每次进入锁屏都会先显示它。"
                      : "如果不固定，锁屏会按本地库顺序每次展示下一张。"}
                  </p>
                  <div className="preview-actions preview-actions--stack">
                    {selectedLocalWallpaper.isFixed ? (
                      <button
                        className="btn btn--soft"
                        type="button"
                        onClick={() => void handleClearFixedWallpaper()}
                        disabled={wallpaperActionPending !== ""}
                      >
                        {wallpaperActionPending === "clear-fixed"
                          ? "处理中..."
                          : "取消固定"}
                      </button>
                    ) : (
                      <button
                        className="btn btn--primary"
                        type="button"
                        onClick={() =>
                          void handleFixWallpaper(selectedLocalWallpaper.sourceUrl)
                        }
                        disabled={wallpaperActionPending !== ""}
                      >
                        {wallpaperActionPending === "fixed-local"
                          ? "处理中..."
                          : "固定这张壁纸"}
                      </button>
                    )}
                    <button
                      className="btn btn--ghost"
                      type="button"
                      onClick={() =>
                        void handleDeleteLocalWallpaper(
                          selectedLocalWallpaper.sourceUrl,
                        )
                      }
                      disabled={wallpaperActionPending !== ""}
                    >
                      {wallpaperActionPending === "delete"
                        ? "删除中..."
                        : "删除这张壁纸"}
                    </button>
                  </div>
                </div>
              </div>
            )}

            <div className="wallpaper-grid wallpaper-grid--local">
              {localWallpapers.map((item) => (
                <button
                  key={item.sourceUrl}
                  className={`wallpaper-tile ${
                    selectedLocalWallpaper?.sourceUrl === item.sourceUrl
                      ? "wallpaper-tile--selected"
                      : ""
                  }`}
                  type="button"
                  onClick={() => setSelectedLocalWallpaperSource(item.sourceUrl)}
                >
                  <img
                    src={convertFileSrc(item.path)}
                    alt={item.authorName || "本地壁纸"}
                  />
                  <div className="wallpaper-tile__meta">
                    <strong>
                      {item.isFixed ? "固定壁纸" : item.authorName || "本地缓存"}
                    </strong>
                    <span>{formatAddedAt(item.addedAt)}</span>
                    <span>{getLocalSourceLabel(item.sourceKind)}</span>
                  </div>
                </button>
              ))}
            </div>
          </>
        )}
      </div>
    </section>
  );

  return (
    <div className="app">
      {!isLockWindow && (
        <>
          <div className="ambient ambient--one" />
          <div className="ambient ambient--two" />
          <div className="ambient ambient--grid" />

          <header className="topbar">
            <div className="brand">
              <img className="brand__icon" src="/huyanba-mark.svg" alt="" />
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

          <div className="view-switcher">
            <button
              className={`view-tab ${
                activeView === "dashboard" ? "view-tab--active" : ""
              }`}
              type="button"
              onClick={() => setActiveView("dashboard")}
            >
              护眼首页
            </button>
            <button
              className={`view-tab ${
                activeView === "wallpapers" ? "view-tab--active" : ""
              }`}
              type="button"
              onClick={() => setActiveView("wallpapers")}
            >
              壁纸控制台
            </button>
          </div>

          {activeView === "dashboard" ? (
            <>
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
                    <button
                      className="btn btn--primary"
                      type="button"
                      onClick={handleStartRest}
                    >
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
                      onChange={(event) =>
                        setFilterStrength(Number(event.target.value))
                      }
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
                        onChange={(event) =>
                          setRestMinutes(Number(event.target.value))
                        }
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
                        onChange={(event) =>
                          setRestDuration(Number(event.target.value))
                        }
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

              <section className="preview-row preview-row--single">
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
          ) : (
            wallpaperConsoleContent
          )}
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
