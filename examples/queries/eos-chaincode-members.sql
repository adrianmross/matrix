select component,
  version,
  physical_chaincode,
  logical_chaincode,
  channel,
  network,
  digest,
  services,
  fact_version as eos_version,
  fact_status as status
from members
where fact_id = 'release-bundle.api.1.0.0'
  and coalesce(physical_chaincode, chaincode) is not null
order by component, version;
