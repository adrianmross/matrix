select c.component as chaincode,
  c.version as chaincode_version,
  c.repo as chaincode_repo,
  c.capability,
  c.capability_version,
  r.component as consumer,
  r.version as consumer_version,
  r.repo as consumer_repo,
  coalesce(c.status, r.status) as status
from capabilities c
join requirements r
  on r.capability = c.capability
 and (r.capability_version is null or r.capability_version = c.capability_version)
where c.type = 'chaincode'
  and (r.component = 'athena' or r.repo like '%/athena')
order by c.component, c.version, r.version;
