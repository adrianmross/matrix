create view api_bundle as
select component, version, runtime, platform, services
from members
where fact_id = 'release-bundle.api.1.0.0';

create view api_bundle_edges as
select edge, target, target_version, runtime, platform
from deref
where fact_id = 'release-bundle.api.1.0.0';
