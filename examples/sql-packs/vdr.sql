create view vdr_tuple as
select component, version, physical_chaincode, channel, network, services
from members
where fact_id = 'smart-contract-tuple.vdr.0.1.0';

create view vdr_edges as
select edge, target, target_version, physical_chaincode, channel, network
from deref
where fact_id = 'smart-contract-tuple.vdr.0.1.0';
