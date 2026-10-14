// Static demo: the theme chosen in the dashboard is kept in this browser only.
try {
  const theme = localStorage.getItem('llca-theme');
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
} catch (e) { /* storage unavailable: follow the system */ }
