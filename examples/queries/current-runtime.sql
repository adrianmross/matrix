select id,
  zone,
  type,
  component,
  version,
  repo,
  status,
  observed_at
from current
order by observed_at desc
limit 50;
