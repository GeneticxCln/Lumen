import { useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Channel, playUrl } from "../lib/tauri";
import { useChannels } from "../hooks/useChannels";
import { t } from "../i18n/index.ts";

interface ChannelListProps {
  onPlayChannel?: (channel: Channel) => void;
  refreshTrigger?: number;
}

export function ChannelList({ onPlayChannel, refreshTrigger = 0 }: ChannelListProps) {
  const {
    playlists,
    selectedPlaylistId,
    selectPlaylist,
    channels,
    groups,
    selectedGroup,
    setSelectedGroup,
    searchQuery,
    setSearchQuery,
    favoritesOnly,
    setFavoritesOnly,
    favoriteIds,
    loading,
    probing,
    handleProbeVisible,
    handleToggleFavorite,
    handleDeletePlaylist,
  } = useChannels(refreshTrigger);

  const listRef = useRef<HTMLDivElement>(null);

  const rowVirtualizer = useVirtualizer({
    count: channels.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 48,
    overscan: 12,
  });

  const handleChannelClick = async (channel: Channel) => {
    if (onPlayChannel) {
      onPlayChannel(channel);
    } else {
      await playUrl(channel.url, undefined, channel.id);
    }
  };

  return (
    <div className="channel-list">
      <div className="playlist-selector">
        <div className="playlist-header-row">
          <label>{t("channels.playlistCount", { count: playlists.length })}</label>
          {selectedPlaylistId && (
            <button
              className="btn-icon-small"
              title={t("channels.deletePlaylist")}
              onClick={() => handleDeletePlaylist(selectedPlaylistId)}
            >
              ✕
            </button>
          )}
        </div>
        <select
          value={selectedPlaylistId || ""}
          onChange={(e) => selectPlaylist(Number(e.target.value))}
          disabled={playlists.length === 0}
        >
          {playlists.length === 0 ? (
            <option value="">{t("channels.noPlaylists")}</option>
          ) : (
            playlists.map((p) => (
              <option key={p.id} value={p.id}>
                {t("channels.playlistOption", { name: p.name, source: p.source })}
              </option>
            ))
          )}
        </select>
      </div>

      <div className="channel-filters">
        <input
          type="text"
          placeholder={t("channels.search")}
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          className="search-input"
        />
        <div className="filter-row">
          <select
            value={selectedGroup}
            onChange={(e) => setSelectedGroup(e.target.value)}
          >
            <option value="all">{t("channels.allGroups", { count: groups.length })}</option>
            {groups.map((g) => (
              <option key={g} value={g}>
                {g}
              </option>
            ))}
          </select>
          <button
            type="button"
            className={`filter-chip ${favoritesOnly ? "active" : ""}`}
            onClick={() => setFavoritesOnly((v) => !v)}
            title={t("channels.favoritesOnly")}
          >
            ★
          </button>
          <button
            type="button"
            className="filter-chip"
            onClick={() => handleProbeVisible()}
            disabled={probing || channels.length === 0}
            title={t("channels.probeTitle")}
          >
            {probing ? "…" : t("channels.probe")}
          </button>
        </div>
      </div>

      <div className="channel-list-container" ref={listRef}>
        {loading ? (
          <div className="loading">{t("channels.loading")}</div>
        ) : channels.length === 0 ? (
          <div className="empty-channels">
            {playlists.length === 0
              ? t("channels.emptyNoPlaylists")
              : t("channels.emptyNoMatch")}
          </div>
        ) : (
          <div
            style={{
              height: `${rowVirtualizer.getTotalSize()}px`,
              width: "100%",
              position: "relative",
            }}
          >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const channel = channels[virtualRow.index];
              const isFav = favoriteIds.has(channel.id);
              return (
                <div
                  key={channel.id ?? virtualRow.index}
                  className="channel-item"
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    height: `${virtualRow.size}px`,
                    transform: `translateY(${virtualRow.start}px)`,
                  }}
                  onClick={() => handleChannelClick(channel)}
                >
                  <button
                    type="button"
                    className={`fav-btn ${isFav ? "active" : ""}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      handleToggleFavorite(channel.id);
                    }}
                    title={isFav ? t("channels.removeFavorite") : t("channels.addFavorite")}
                  >
                    {isFav ? "★" : "☆"}
                  </button>
                  {channel.tvg_logo && (
                    <img
                      src={channel.tvg_logo}
                      alt=""
                      className="channel-logo"
                      loading="lazy"
                      decoding="async"
                      onError={(e) => {
                        (e.target as HTMLImageElement).style.display = "none";
                      }}
                    />
                  )}
                  <span className="channel-name" title={channel.name}>
                    {channel.name}
                  </span>
                  {channel.group_title && (
                    <span className="channel-group">{channel.group_title}</span>
                  )}
                  {channel.last_ok ? (
                    <span className="live-indicator" title={t("channels.live")}>
                      ●
                    </span>
                  ) : channel.last_error ? (
                    <span
                      className="dead-indicator"
                      title={t("channels.offline", { reason: channel.last_error })}
                    >
                      ●
                    </span>
                  ) : null}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
