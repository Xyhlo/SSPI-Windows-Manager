import React, { useState } from 'react'
import { Terminal, ChevronRight, Globe, Languages } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '../../utils/helpers'

const FOLLOW_BROWSER_LANGUAGE = '__auto__'

const isFollowingBrowserLanguage = () => {
  try {
    return !localStorage.getItem('i18nextLng')
  } catch {
    return true
  }
}

const SettingRow = ({ title, description, children }) => <div className="sspi-setting-row"><div className="sspi-setting-copy"><h3>{title}</h3><p>{description}</p></div><div className="sspi-setting-control">{children}</div></div>
const Toggle = ({ label, value, onChange }) => <button className="sspi-switch" type="button" role="switch" aria-label={label} aria-checked={!!value} onClick={onChange}><span /></button>

const SettingsView = ({ config, onSaveConfig, setShowLogs, onNavigate }) => {
  const { t, i18n } = useTranslation()
  const autoOpen = config.AUTO_BROWSER_OPEN !== false
  const autoloadDelay = config.AUTOLOAD_DELAY || 5
  const multiSources = config.MULTI_SOURCES_ENABLED === true
  const [followBrowserLanguage, setFollowBrowserLanguage] = useState(isFollowingBrowserLanguage)

  const getLanguageDisplayName = (lang) => {
    let displayName = lang;
    try {
      const baseLang = lang.split('-')[0];
      const lookupLang = lang.startsWith('zh') ? lang : baseLang;
      displayName = new Intl.DisplayNames([lang], { type: 'language' }).of(lookupLang);
      displayName = displayName.charAt(0).toUpperCase() + displayName.slice(1);
    } catch {
      return lang;
    }
    return displayName;
  };

  const currentLang = i18n.resolvedLanguage || i18n.language || 'en';
  const selectedLanguage = followBrowserLanguage ? FOLLOW_BROWSER_LANGUAGE : currentLang;

  const handleLanguageChange = async (event) => {
    const language = event.target.value;
    const followBrowser = language === FOLLOW_BROWSER_LANGUAGE;
    setFollowBrowserLanguage(followBrowser);
    try {
      if (followBrowser) {
        localStorage.removeItem('i18nextLng');
      } else {
        localStorage.setItem('i18nextLng', language);
      }
    } catch {
      // Continue with an in-memory language change if storage is unavailable.
    }
    await i18n.changeLanguage(followBrowser ? undefined : language);
  };

  return (
    <div className="sspi-page sspi-settings">
      <div className="sspi-page-heading"><h2>{t('settings.title', 'Settings')}</h2></div>
      <section className="sspi-section">
        <SettingRow title={t('settings.language_title', 'Language')} description={t('settings.language_disclaimer', 'Translations are community-driven and may contain errors.')}>
          <select className="sspi-select" value={selectedLanguage} onChange={handleLanguageChange} aria-label={t('settings.language_title', 'Language')}><option value={FOLLOW_BROWSER_LANGUAGE}>{t('settings.language_system_default', 'System Default')}</option>{Object.keys(i18n.store.data).map(lang => <option key={lang} value={lang}>{getLanguageDisplayName(lang)}</option>)}</select>
        </SettingRow>
        <SettingRow title={t('settings.auto_open_title', 'Auto-open Browser')} description={t('settings.auto_open_desc', 'Automatically launch the browser when Payload Manager payload is executed.')}><Toggle label={t('settings.auto_open_title', 'Auto-open Browser')} value={autoOpen} onChange={() => onSaveConfig({ AUTO_BROWSER_OPEN: !autoOpen })} /></SettingRow>
        <SettingRow title={t('sspi.home_entry', 'Home-screen entry')} description={t('sspi.home_entry_description', 'SSPI setup installs one entry. Reopen it to continue in Payload Manager when this console session is ready.')}><span className="sspi-muted">SSPI</span></SettingRow>
        <SettingRow title={t('settings.kill_disc_title', 'Kill Disc Player')} description={t('settings.kill_disc_desc', 'Automatically terminate the Disc Player application on startup (for BD-JB users).')}><Toggle label={t('settings.kill_disc_title', 'Kill Disc Player')} value={config.KILL_DISC_PLAYER_ON_STARTUP !== false} onChange={() => onSaveConfig({ KILL_DISC_PLAYER_ON_STARTUP: !config.KILL_DISC_PLAYER_ON_STARTUP })} /></SettingRow>
        <SettingRow title={t('settings.scan_usb_title', 'Scan USB Payloads')} description={t('settings.scan_usb_desc', 'Enable scanning for .elf and .bin files in the root directory of USB drives (/mnt/usb0-7).')}><Toggle label={t('settings.scan_usb_title', 'Scan USB Payloads')} value={config.SCAN_USB_PAYLOADS} onChange={() => onSaveConfig({ SCAN_USB_PAYLOADS: !config.SCAN_USB_PAYLOADS })} /></SettingRow>
        <SettingRow title={t('settings.autoload_delay_title', 'Autoload Delay')} description={t('settings.autoload_delay_desc', 'Wait time before the autoload sequence begins.')}><div className="sspi-segments">{[3, 5, 10].map(seconds => <button key={seconds} className={autoloadDelay === seconds ? 'selected' : ''} aria-pressed={autoloadDelay === seconds} onClick={() => onSaveConfig({ AUTOLOAD_DELAY: seconds })}>{seconds}s</button>)}</div></SettingRow>
      </section>
      <section className="sspi-section"><h3>{t('settings.sources_title', 'Payload Sources')}</h3>
        <SettingRow title={t('settings.multi_sources_title', 'Multiple Payload Sources')} description={t('settings.multi_sources_desc', 'Enable third-party payload repositories. Payloads from multiple sources are grouped by catalog in the Manage tab.')}><Toggle label={t('settings.multi_sources_title', 'Multiple Payload Sources')} value={multiSources} onChange={() => onSaveConfig({ MULTI_SOURCES_ENABLED: !multiSources })} /></SettingRow>
        {multiSources && <button className="sspi-data-row interactive" onClick={() => onNavigate('sources')}><Globe size={21} /><span className="sspi-grow"><strong>{t('settings.manage_sources_title', 'Manage Sources')}</strong><small>{t('settings.manage_sources_desc', 'Add, remove, or reorder your payload repositories.')}</small></span><ChevronRight size={20} /></button>}
      </section>
      <section className="sspi-section"><h3>{t('settings.diagnostics_title', 'Diagnostics')}</h3><button className="sspi-data-row interactive" onClick={() => setShowLogs(true)}><Terminal size={21} /><span className="sspi-grow"><strong>{t('settings.log_viewer_title', 'Open Log Viewer')}</strong><small>{t('settings.log_viewer_desc', 'Access real-time debug output from the Payload Manager daemon.')}</small></span><ChevronRight size={20} /></button></section>
    </div>
  )
}

export default SettingsView
