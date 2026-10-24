// PAC standard helpers. Native dnsResolve/myIpAddress are supplied by the host.
function isPlainHostName(host) { return String(host).indexOf('.') < 0; }
function dnsDomainIs(host, domain) { return String(host).endsWith(String(domain)); }
function localHostOrDomainIs(host, hostdom) { return host === hostdom || (isPlainHostName(host) && String(hostdom).startsWith(host + '.')); }
function dnsDomainLevels(host) { return (String(host).match(/\./g) || []).length; }
function isResolvable(host) { return dnsResolve(host) !== null; }
function shExpMatch(text, pattern) {
    const escaped = String(pattern).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.');
    return new RegExp('^' + escaped + '$').test(String(text));
}
function isInNet(host, network, mask) {
    function ipv4(text) {
        const parts = String(text).split('.');
        if (parts.length !== 4 || parts.some(p => !/^\d{1,3}$/.test(p) || +p > 255)) return null;
        return parts.reduce((v, p) => ((v << 8) | +p), 0);
    }
    let address = ipv4(host);
    if (address === null) { const resolved = dnsResolve(host); if (resolved === null) return false; address = ipv4(resolved); }
    const net = ipv4(network), bits = ipv4(mask);
    return address !== null && net !== null && bits !== null && ((address & bits) === (net & bits));
}
function _pacArgs(args) { const a = Array.from(args); const utc = a[a.length - 1] === 'GMT'; if (utc) a.pop(); return [a, utc]; }
function _pacBetween(value, start, end) { return start <= end ? value >= start && value <= end : value >= start || value <= end; }
function weekdayRange() {
    const [a, utc] = _pacArgs(arguments), names = ['SUN','MON','TUE','WED','THU','FRI','SAT'];
    if (a.length < 1 || a.length > 2) return false;
    const first = names.indexOf(a[0]), last = names.indexOf(a[a.length - 1]);
    const date = new Date(), now = utc ? date.getUTCDay() : date.getDay();
    return first >= 0 && last >= 0 && _pacBetween(now, first, last);
}
function timeRange() {
    const [a, utc] = _pacArgs(arguments), date = new Date();
    const hour = utc ? date.getUTCHours() : date.getHours(), minute = utc ? date.getUTCMinutes() : date.getMinutes(), second = utc ? date.getUTCSeconds() : date.getSeconds();
    if (a.some(v => !Number.isInteger(v))) return false;
    if (a.length === 1) return hour === a[0];
    if (a.length === 2) return _pacBetween(hour, a[0], a[1]);
    if (a.length === 4) return _pacBetween(hour*60+minute, a[0]*60+a[1], a[2]*60+a[3]);
    if (a.length === 6) return _pacBetween(hour*3600+minute*60+second, a[0]*3600+a[1]*60+a[2], a[3]*3600+a[4]*60+a[5]);
    return false;
}
function dateRange() {
    const [a, utc] = _pacArgs(arguments), date = new Date(), months = ['JAN','FEB','MAR','APR','MAY','JUN','JUL','AUG','SEP','OCT','NOV','DEC'];
    const day = utc ? date.getUTCDate() : date.getDate(), month = utc ? date.getUTCMonth() : date.getMonth(), year = utc ? date.getUTCFullYear() : date.getFullYear();
    const mon = value => months.indexOf(value);
    if (a.length === 1 || a.length === 2) {
        const x = a[0], y = a[a.length-1];
        if (typeof x === 'string') return mon(x) >= 0 && mon(y) >= 0 && _pacBetween(month, mon(x), mon(y));
        if (x > 31) return _pacBetween(year, x, y);
        return _pacBetween(day, x, y);
    }
    if (a.length === 4 && typeof a[0] === 'string') return mon(a[0]) >= 0 && mon(a[2]) >= 0 && _pacBetween(year*12+month,a[1]*12+mon(a[0]),a[3]*12+mon(a[2]));
    if (a.length === 4) return mon(a[1]) >= 0 && mon(a[3]) >= 0 && _pacBetween(month*32+day,mon(a[1])*32+a[0],mon(a[3])*32+a[2]);
    if (a.length === 6) return mon(a[1]) >= 0 && mon(a[4]) >= 0 && _pacBetween(year*512+month*32+day,a[2]*512+mon(a[1])*32+a[0],a[5]*512+mon(a[4])*32+a[3]);
    return false;
}
function alert(_message) { /* PAC messages never become application logs. */ }
