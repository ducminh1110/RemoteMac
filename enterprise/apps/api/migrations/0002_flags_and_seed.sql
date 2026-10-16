-- Operational kill switches, on by default (operators turn them off in an incident).
INSERT INTO feature_flags (key, description, enabled) VALUES
    ('rentals.new', 'New rentals can be created (off: the catalog shows rentals as paused)', true),
    ('billing.checkout', 'Checkout sessions can be started', true),
    ('connect.grants', 'Connection grants can be issued', true),
    ('provisioning.worker', 'The provisioning worker takes new jobs', true);
